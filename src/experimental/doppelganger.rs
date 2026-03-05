//! Doppelganger: Structural Twins, Semantic Strangers.
//!
//! "Who is doing the exact same thing as you, but for completely different reasons?"
//!
//! The Doppelganger Detector identifies pairs of nodes that have highly similar structural
//! neighborhoods (e.g., they connect to the same nodes) but are distant or contradictory
//! in semantic vector space.
//!
//! # Use Cases
//! - **Fraud Detection**: Finding accounts that mimic normal user structure but have anomalous content.
//! - **Role Discovery**: Identifying nodes that play the exact same role in different semantic domains.
//! - **Contradiction Detection**: Finding entities that agree on relationships but disagree on meaning.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::DoppelgangerDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id_1 = db.create_node("User", Default::default())?;
//! # let node_id_2 = db.create_node("User", Default::default())?;
//! let detector = DoppelgangerDetector::new(&db);
//!
//! // Check candidates for doppelgangers
//! let candidates = vec![node_id_1, node_id_2]; // Populate with actual IDs
//!
//! // structural_threshold = 0.8 (highly similar structure)
//! // semantic_distance_threshold = 0.9 (far apart semantically)
//! let doppelgangers = detector.find_doppelgangers(&candidates, "embedding", 0.8, 0.9)?;
//!
//! for d in doppelgangers {
//!     println!("Found doppelganger pair {} and {} (Structural Sim: {:.2}, Semantic Dist: {:.2})",
//!         d.node_a, d.node_b, d.structural_similarity, d.semantic_distance);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashSet;

/// A detected doppelganger pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Doppelganger {
    /// The first node in the pair.
    pub node_a: NodeId,
    /// The second node in the pair.
    pub node_b: NodeId,
    /// The Jaccard similarity of their structural neighborhoods.
    pub structural_similarity: f32,
    /// The semantic distance (e.g., cosine distance) between their vectors.
    pub semantic_distance: f32,
}

/// Detector for finding semantic doppelgangers.
pub struct DoppelgangerDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerDetector<'a> {
    /// Create a new DoppelgangerDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find doppelgangers among a set of candidate nodes.
    ///
    /// # Arguments
    ///
    /// * `candidates` - List of nodes to check.
    /// * `property_name` - The vector property to use for semantic comparison.
    /// * `structural_threshold` - Minimum Jaccard similarity of neighborhoods (0.0 to 1.0).
    /// * `semantic_distance_threshold` - Minimum semantic distance (0.0 to 2.0 for cosine distance)
    ///   to be considered a doppelganger.
    pub fn find_doppelgangers(
        &self,
        candidates: &[NodeId],
        property_name: &str,
        structural_threshold: f32,
        semantic_distance_threshold: f32,
    ) -> Result<Vec<Doppelganger>> {
        let mut doppelgangers = Vec::new();

        // Cache vectors and neighborhoods for candidates
        let mut candidate_data = Vec::with_capacity(candidates.len());

        for &node_id in candidates {
            let node = match self.db.get_node(node_id) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let vec = match node
                .properties
                .get(property_name)
                .and_then(|p| p.as_vector())
            {
                Some(v) => v.to_vec(),
                None => continue,
            };

            let neighborhood = self.get_neighborhood(node_id)?;
            candidate_data.push((node_id, vec, neighborhood));
        }

        // Compare all pairs of candidates
        for i in 0..candidate_data.len() {
            for j in (i + 1)..candidate_data.len() {
                let (node_a, vec_a, neighbors_a) = &candidate_data[i];
                let (node_b, vec_b, neighbors_b) = &candidate_data[j];

                // 1. Check structural similarity (Jaccard)
                let structural_similarity = Self::jaccard_similarity(neighbors_a, neighbors_b);

                if structural_similarity >= structural_threshold {
                    // 2. Check semantic distance
                    let similarity = ops::cosine_similarity(vec_a, vec_b)?;
                    let distance = 1.0 - similarity; // Cosine distance

                    if distance >= semantic_distance_threshold {
                        doppelgangers.push(Doppelganger {
                            node_a: *node_a,
                            node_b: *node_b,
                            structural_similarity,
                            semantic_distance: distance,
                        });
                    }
                }
            }
        }

        Ok(doppelgangers)
    }

    /// Helper to gather the neighborhood of a node (incoming and outgoing edges).
    fn get_neighborhood(&self, node_id: NodeId) -> Result<HashSet<NodeId>> {
        let mut neighbors = HashSet::new();

        let outgoing = self.db.get_outgoing_edges(node_id);
        for edge_id in outgoing {
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                neighbors.insert(target);
            }
        }

        let incoming = self.db.get_incoming_edges(node_id);
        for edge_id in incoming {
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                neighbors.insert(source);
            }
        }

        Ok(neighbors)
    }

    /// Calculate Jaccard similarity between two sets.
    fn jaccard_similarity(set_a: &HashSet<NodeId>, set_b: &HashSet<NodeId>) -> f32 {
        if set_a.is_empty() && set_b.is_empty() {
            return 1.0;
        }

        let intersection = set_a.intersection(set_b).count() as f32;
        let union = set_a.union(set_b).count() as f32;

        if union == 0.0 {
            0.0
        } else {
            intersection / union
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_doppelganger_detection() {
        let db = AletheiaDB::new().unwrap();

        // Node 1: User A
        let p1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let mut u1 = NodeId::new(0).unwrap();

        // Node 2: User B (Doppelganger of A - same structure, different vector)
        let p2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let mut u2 = NodeId::new(0).unwrap();

        // Node 3: Target C
        let p3 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.5, 0.5])
            .build();
        let mut t3 = NodeId::new(0).unwrap();

        // Node 4: Target D
        let p4 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.5, 0.5])
            .build();
        let mut t4 = NodeId::new(0).unwrap();

        db.write(|tx| {
            u1 = tx.create_node("User", p1).unwrap();
            u2 = tx.create_node("User", p2).unwrap();
            t3 = tx.create_node("Target", p3).unwrap();
            t4 = tx.create_node("Target", p4).unwrap();

            // Both U1 and U2 connect to T3 and T4
            tx.create_edge(u1, t3, "LIKES", Default::default()).unwrap();
            tx.create_edge(u1, t4, "LIKES", Default::default()).unwrap();
            tx.create_edge(u2, t3, "LIKES", Default::default()).unwrap();
            tx.create_edge(u2, t4, "LIKES", Default::default()).unwrap();

            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let detector = DoppelgangerDetector::new(&db);
        let candidates = vec![u1, u2];

        // Find doppelgangers: high structural overlap (>= 0.8), semantic distance >= 0.5
        let doppelgangers = detector
            .find_doppelgangers(&candidates, "embedding", 0.8, 0.5)
            .unwrap();

        assert_eq!(doppelgangers.len(), 1);
        let d = &doppelgangers[0];
        assert_eq!(d.node_a, u1);
        assert_eq!(d.node_b, u2);

        // They connect to exactly the same nodes (T3, T4), so structural sim is 1.0
        assert!((d.structural_similarity - 1.0).abs() < f32::EPSILON);

        // Semantic distance for orthogonal vectors [1,0] and [0,1] should be 1.0
        assert!((d.semantic_distance - 1.0).abs() < f32::EPSILON);
    }
}
