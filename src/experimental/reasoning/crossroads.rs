//! Crossroads: Semantic Bridge Discovery Engine.
//!
//! "Innovation happens at the intersections."
//!
//! The Crossroads Engine finds nodes that act as semantic bridges between two
//! completely orthogonal (unrelated) concepts. In a knowledge graph, these nodes
//! represent interdisciplinary ideas, hybrid technologies, or people who connect
//! distinct social circles.
//!
//! # Concepts
//! - **Orthogonality**: Two vectors are orthogonal if their cosine similarity is near 0 (or negative).
//! - **Crossroad Node**: A node whose vector is simultaneously similar to Concept A *and* Concept B.
//! - **Bridge Score**: We calculate this by clamping the similarities to `[0, 1]` and taking
//!   their minimum. A high minimum means the node balances both concepts well.
//!
//! # Use Cases
//! - **RAG / Discovery**: "What technology bridges 'Biology' and 'Computer Science'?"
//! - **Innovation**: Finding patents that merge two distinct domains.
//! - **Social Dynamics**: Finding brokers between two isolated communities.

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::cmp::Ordering;

/// Result of a Crossroads analysis for a specific node.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossroadResult {
    /// The node acting as a crossroad.
    pub node_id: NodeId,
    /// Similarity to Concept A.
    pub similarity_a: f32,
    /// Similarity to Concept B.
    pub similarity_b: f32,
    /// The overall crossroad score (min of sim_a and sim_b).
    pub bridge_score: f32,
}

/// The Crossroads Engine for semantic bridge discovery.
pub struct CrossroadsDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> CrossroadsDetector<'a> {
    /// Create a new CrossroadsDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find nodes that bridge two conceptual vectors.
    ///
    /// # Arguments
    /// * `concept_a` - The first semantic vector.
    /// * `concept_b` - The second semantic vector.
    /// * `property_name` - The vector property to analyze (e.g., "embedding").
    /// * `limit` - Maximum number of results to return.
    ///
    /// # Returns
    /// A list of `CrossroadResult`s, sorted by `bridge_score` in descending order.
    pub fn find_crossroads(
        &self,
        concept_a: &[f32],
        concept_b: &[f32],
        property_name: &str,
        limit: usize,
    ) -> Result<Vec<CrossroadResult>> {
        // Ensure inputs are valid and not empty
        if concept_a.is_empty() || concept_b.is_empty() {
            return Err(Error::other("Concept vectors cannot be empty"));
        }

        if concept_a.len() != concept_b.len() {
            return Err(Error::other(
                "Concept A and Concept B must have the same dimensionality",
            ));
        }

        // Normalize the concept vectors so we can just dot product later,
        // or we can just use our standard cosine_similarity function.
        let mut norm_a = concept_a.to_vec();
        let mut norm_b = concept_b.to_vec();
        ops::normalize_in_place(&mut norm_a);
        ops::normalize_in_place(&mut norm_b);

        let mut results = Vec::new();

        // Ideally we'd use an index, but finding intersections requires evaluating both.
        // A full scan is necessary unless we have a composite index.
        let scan_results = self.db.query().scan(None).execute(self.db)?;

        #[allow(clippy::collapsible_if)]
        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                if let Some(vec_prop) = node.get_property(property_name) {
                    if let Some(v) = vec_prop.as_vector() {
                        // Ensure dimensions match
                        if v.len() == norm_a.len() {
                            let mut norm_v = v.to_vec();
                            ops::normalize_in_place(&mut norm_v);

                            // Use dot product since vectors are normalized
                            let mut sim_a = 0.0_f32;
                            let mut sim_b = 0.0_f32;
                            for i in 0..v.len() {
                                sim_a += norm_v[i] * norm_a[i];
                                sim_b += norm_v[i] * norm_b[i];
                            }

                            // Clamp similarities to [0, 1] to avoid penalizing or rewarding negative similarities
                            let clamp_a = sim_a.clamp(0.0, 1.0);
                            let clamp_b = sim_b.clamp(0.0, 1.0);

                            // The bridge score is the minimum of the two clamped similarities.
                            // If a node is 0.9 to A and 0.1 to B, its score is 0.1 (poor bridge).
                            // If a node is 0.5 to A and 0.5 to B, its score is 0.5 (good bridge).
                            let bridge_score = clamp_a.min(clamp_b);

                            // Optimization: Only add if it has SOME bridging capacity
                            if bridge_score > 0.01 {
                                results.push(CrossroadResult {
                                    node_id: node.id,
                                    similarity_a: sim_a,
                                    similarity_b: sim_b,
                                    bridge_score,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Sort by bridge score descending
        results.sort_by(|a, b| {
            b.bridge_score
                .partial_cmp(&a.bridge_score)
                .unwrap_or(Ordering::Equal)
        });

        // Apply limit
        if results.len() > limit {
            results.truncate(limit);
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_find_crossroads() {
        let db = AletheiaDB::new().unwrap();

        // Concept A (e.g., Biology): [1.0, 0.0, 0.0]
        let concept_a = vec![1.0, 0.0, 0.0];
        // Concept B (e.g., Computer Science): [0.0, 1.0, 0.0]
        let concept_b = vec![0.0, 1.0, 0.0];

        // Ensure they are orthogonal
        let mut ca = concept_a.clone();
        let mut cb = concept_b.clone();
        ops::normalize_in_place(&mut ca);
        ops::normalize_in_place(&mut cb);
        assert_eq!(ops::cosine_similarity(&ca, &cb).unwrap(), 0.0);

        let mut node_bio = NodeId::new(0).unwrap();
        let mut node_cs = NodeId::new(0).unwrap();
        let mut node_bridge = NodeId::new(0).unwrap();
        let mut node_irrelevant = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Node strongly aligned with A
            node_bio = tx
                .create_node(
                    "Domain",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.9, 0.1, 0.0])
                        .build(),
                )
                .unwrap();

            // Node strongly aligned with B
            node_cs = tx
                .create_node(
                    "Domain",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.1, 0.9, 0.0])
                        .build(),
                )
                .unwrap();

            // Node bridging A and B (Bioinformatics!)
            node_bridge = tx
                .create_node(
                    "Domain",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.707, 0.707, 0.0])
                        .build(),
                )
                .unwrap();

            // Irrelevant node
            node_irrelevant = tx
                .create_node(
                    "Domain",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0, 1.0])
                        .build(),
                )
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = CrossroadsDetector::new(&db);
        let results = detector
            .find_crossroads(&concept_a, &concept_b, "vec", 10)
            .unwrap();

        assert!(!results.is_empty());

        // The bridge node should have the highest bridge score
        assert_eq!(results[0].node_id, node_bridge);

        // Bridge score should be roughly ~0.707 (since 0.707/1.0 = 0.707 similarity to both normalized axes)
        assert!(results[0].bridge_score > 0.65 && results[0].bridge_score < 0.75);

        // Irrelevant node should NOT be in the results (score < 0.01)
        assert!(!results.iter().any(|r| r.node_id == node_irrelevant));
    }
}
