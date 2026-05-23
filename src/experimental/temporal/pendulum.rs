//! Pendulum: Semantic Oscillation Detector.
//!
//! "Why does this keep going back and forth?"
//!
//! The Pendulum Engine detects nodes that exhibit oscillatory behavior in semantic space—
//! i.e., moving away from a concept only to return to it later.
//!
//! # Concepts
//! - **Oscillation**: The act of moving from State A -> State B -> State A.
//! - **Reversion**: When a vector becomes highly similar to an older historical version of itself,
//!   after having drifted away in between.
//!
//! # Use Cases
//! - **Indecision Detection**: Identifying LLM agents that are stuck in a loop or flip-flopping.
//! - **A/B Testing Flip-Flops**: Recognizing when users revert to old configurations repeatedly.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::pendulum::Pendulum;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = NodeId::new(1)?;
//! let pendulum = Pendulum::new(&db);
//!
//! // Detect if the node's embedding swung away and then reverted back
//! // with at least 0.95 similarity to the old state, and at least 0.2 divergence during the swing.
//! let events = pendulum.detect_oscillation(node_id, "embedding", 0.95, 0.2)?;
//!
//! for event in events {
//!     println!("Reverted to version {} at version {}!", event.historical_version, event.reversion_version);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The result of an oscillation check.
#[derive(Debug, Clone)]
pub struct OscillationEvent {
    /// The version number where the node reverted.
    pub reversion_version: u64,
    /// The older version number it reverted to.
    pub historical_version: u64,
    /// How similar the reversion is to the historical state.
    pub similarity_score: f32,
    /// The maximum divergence that happened *between* these two versions.
    pub swing_magnitude: f32,
}

/// The Pendulum Engine.
pub struct Pendulum<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Pendulum<'a> {
    /// Create a new Pendulum Engine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node is oscillating back and forth in vector space.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `similarity_threshold` - How close the current vector needs to be to an older vector
    ///   to be considered a "reversion" (e.g., 0.95).
    /// * `min_swing_magnitude` - The minimum divergence (1.0 - similarity) that must have occurred
    ///   between the older version and the current version to prove it actually swung away.
    pub fn detect_oscillation(
        &self,
        node_id: NodeId,
        property_name: &str,
        similarity_threshold: f32,
        min_swing_magnitude: f32,
    ) -> Result<Vec<OscillationEvent>> {
        let history = self.db.get_node_history(node_id)?;
        let mut events = Vec::new();

        let mut versions_with_vectors = Vec::new();

        for version in &history.versions {
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    versions_with_vectors.push((version.version_number, vec.to_vec()));
                }
            }
        }

        if versions_with_vectors.len() < 3 {
            return Ok(events);
        }

        for i in 2..versions_with_vectors.len() {
            let (curr_v, curr_vec) = &versions_with_vectors[i];

            for j in 0..i - 1 {
                let (old_v, old_vec) = &versions_with_vectors[j];

                let sim = cosine_similarity(curr_vec, old_vec)?;

                if sim >= similarity_threshold {
                    let mut max_swing = 0.0_f32;

                    for k in j + 1..i {
                        let (_, mid_vec) = &versions_with_vectors[k];
                        let mid_sim = cosine_similarity(old_vec, mid_vec)?;
                        let divergence = 1.0 - mid_sim;
                        if divergence > max_swing {
                            max_swing = divergence;
                        }
                    }

                    if max_swing >= min_swing_magnitude {
                        events.push(OscillationEvent {
                            reversion_version: *curr_v,
                            historical_version: *old_v,
                            similarity_score: sim,
                            swing_magnitude: max_swing,
                        });
                        break;
                    }
                }
            }
        }

        Ok(events)
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_pendulum_detects_oscillation() {
        let db = AletheiaDB::new().unwrap();

        // v1: Concept A
        let p1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0, 0.0])
            .build();
        let node_id = db.create_node("Node", p1).unwrap();

        // v2: Concept B (Swings away)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // v3: Concept A again (Reverts)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.14, 0.0]) // ~0.99 similarity to A
                    .build(),
            )
        })
        .unwrap();

        let pendulum = Pendulum::new(&db);
        let events = pendulum
            .detect_oscillation(node_id, "embedding", 0.95, 0.5)
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reversion_version, 3);
        assert_eq!(events[0].historical_version, 1);
        assert!(events[0].similarity_score >= 0.95);
        assert!(events[0].swing_magnitude >= 0.5); // cosine([1,0,0], [0,1,0]) = 0.0 -> divergence = 1.0
    }

    #[test]
    fn test_pendulum_ignores_stable_nodes() {
        let db = AletheiaDB::new().unwrap();

        let p1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0, 0.0])
            .build();
        let node_id = db.create_node("Node", p1).unwrap();

        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.14, 0.0])
                    .build(),
            )
        })
        .unwrap();

        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let pendulum = Pendulum::new(&db);
        // Minimum swing is 0.5, but our drift was very small
        let events = pendulum
            .detect_oscillation(node_id, "embedding", 0.95, 0.5)
            .unwrap();

        assert!(events.is_empty(), "Stable node should not trigger oscillation");
    }
}
