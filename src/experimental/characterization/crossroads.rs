#![allow(clippy::collapsible_if)]

//! Crossroads: Semantic Divergence at Decision Points.
//!
//! "Where do we go from here?"
//!
//! The Crossroads Detector identifies nodes whose outgoing edges lead to
//! radically different semantic spaces. A node with a high divergence score
//! represents a "crossroads" or decision point, where choosing one path
//! completely changes the semantic context compared to another.
//!
//! # Concepts
//! - **Highway**: A node whose neighbors are all semantically similar to each other.
//! - **Crossroads**: A node whose neighbors are semantically distant from each other.
//!
//! # Use Cases
//! - **Story Generation**: Find points in a narrative graph where the plot can branch wildly.
//! - **Recommender Systems**: Identify "exploration" nodes vs "deep dive" nodes.
//! - **User Journey**: Detect when a user is at a point of high uncertainty.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The Crossroads Detector Engine.
pub struct CrossroadsDetector<'a> {
    db: &'a AletheiaDB,
}

/// The result of a Crossroads analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossroadsResult {
    /// The node analyzed.
    pub node_id: NodeId,
    /// The divergence score (average pairwise distance between neighbors).
    /// Higher score = more divergent options = Crossroads.
    pub divergence_score: f32,
    /// Number of valid paths evaluated.
    pub paths_evaluated: usize,
}

impl<'a> CrossroadsDetector<'a> {
    /// Create a new Crossroads Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the divergence score of a node's outgoing neighbors.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    pub fn calculate_divergence(
        &self,
        node_id: NodeId,
        property_name: &str,
    ) -> Result<CrossroadsResult> {
        // 1. Get outgoing edges and collect targets
        let outgoing = self.db.get_outgoing_edges(node_id);

        let mut target_vectors = Vec::new();

        for edge_id in outgoing {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                if let Ok(target) = self.db.get_node(edge.target) {
                    if let Some(prop) = target.properties.get(property_name) {
                        if let Some(vec) = prop.as_vector() {
                            target_vectors.push(vec.to_vec());
                        }
                    }
                }
            }
        }

        // If 0 or 1 valid targets, divergence is 0
        if target_vectors.len() < 2 {
            return Ok(CrossroadsResult {
                node_id,
                divergence_score: 0.0,
                paths_evaluated: target_vectors.len(),
            });
        }

        // 2. Calculate average pairwise distance (1.0 - similarity)
        let mut total_distance = 0.0;
        let mut pair_count = 0;

        for i in 0..target_vectors.len() {
            for j in (i + 1)..target_vectors.len() {
                let sim = cosine_similarity(&target_vectors[i], &target_vectors[j])?;
                // Distance ranges from 0.0 (identical) to 2.0 (opposite)
                // We'll map similarity (1.0 to -1.0) to distance (0.0 to 1.0 or 2.0)
                // Let's use 1.0 - sim (so orthogonal is 1.0, opposite is 2.0)
                let dist = 1.0 - sim;
                total_distance += dist;
                pair_count += 1;
            }
        }

        let divergence_score = total_distance / pair_count as f32;

        Ok(CrossroadsResult {
            node_id,
            divergence_score,
            paths_evaluated: target_vectors.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_crossroads_high_divergence() {
        let db = AletheiaDB::new().unwrap();

        let start_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let start = db.create_node("Node", start_props).unwrap();

        // Option A: Logic
        let a_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", a_props).unwrap();

        // Option B: Emotion
        let b_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", b_props).unwrap();

        db.create_edge(start, a, "CHOICE", Default::default())
            .unwrap();
        db.create_edge(start, b, "CHOICE", Default::default())
            .unwrap();

        let detector = CrossroadsDetector::new(&db);
        let result = detector.calculate_divergence(start, "vec").unwrap();

        // Pairwise similarity of [1,0] and [0,1] is 0.0
        // Distance is 1.0 - 0.0 = 1.0
        assert_eq!(result.paths_evaluated, 2);
        assert!((result.divergence_score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_crossroads_highway() {
        let db = AletheiaDB::new().unwrap();

        let start_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let start = db.create_node("Node", start_props).unwrap();

        // Option A: Logic 1
        let a_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", a_props).unwrap();

        // Option B: Logic 2
        let b_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.99, 0.01])
            .build();
        let b = db.create_node("Node", b_props).unwrap();

        db.create_edge(start, a, "CHOICE", Default::default())
            .unwrap();
        db.create_edge(start, b, "CHOICE", Default::default())
            .unwrap();

        let detector = CrossroadsDetector::new(&db);
        let result = detector.calculate_divergence(start, "vec").unwrap();

        // Very similar, so divergence is near 0.0
        assert_eq!(result.paths_evaluated, 2);
        assert!(result.divergence_score < 0.1);
    }

    #[test]
    fn test_crossroads_dead_end() {
        let db = AletheiaDB::new().unwrap();

        let start_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let start = db.create_node("Node", start_props).unwrap();

        let detector = CrossroadsDetector::new(&db);
        let result = detector.calculate_divergence(start, "vec").unwrap();

        assert_eq!(result.paths_evaluated, 0);
        assert_eq!(result.divergence_score, 0.0);
    }
}
