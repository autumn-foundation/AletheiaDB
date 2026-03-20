#![allow(clippy::collapsible_if)]

//! DejaVu: Semantic Recurrence Detector.
//!
//! "History doesn't repeat itself, but it often rhymes."
//!
//! The DejaVu Engine detects when a node's semantic state (its vector) has reverted
//! to a state it held in the distant past, after having drifted away from it.
//!
//! # Concepts
//! - **Recurrence**: When a node's current vector is highly similar to an older version,
//!   but significantly different from its most recent versions.
//! - **Cycle**: A pattern of semantic drift and return.
//!
//! # Use Cases
//! - **Behavioral Analysis**: "Did this user revert to their old purchasing habits?"
//! - **Topic Tracking**: "Is this news cycle returning to an old narrative?"
//! - **Anomaly Detection**: "Why did this model's embeddings suddenly revert to last week's state?"

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::cosine_similarity;

/// A detected instance of semantic recurrence.
#[derive(Debug, Clone, PartialEq)]
pub struct DejaVuEvent {
    /// The node that experienced Deja Vu.
    pub node_id: NodeId,
    /// The current timestamp.
    pub current_time: Timestamp,
    /// The historical timestamp it reverted to.
    pub past_time: Timestamp,
    /// Similarity between current state and the past state (high).
    pub recurrence_similarity: f32,
    /// Similarity between current state and the immediate predecessor state (low).
    pub recent_divergence: f32,
}

/// The DejaVu Engine for detecting semantic cycles.
pub struct DejaVuEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejaVuEngine<'a> {
    /// Create a new DejaVuEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node has experienced Deja Vu recently.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to check.
    /// * `recurrence_threshold` - The minimum similarity to a past state to be considered a recurrence (e.g., 0.9).
    /// * `divergence_threshold` - The maximum similarity to the immediate predecessor state (e.g., 0.7).
    pub fn detect(
        &self,
        node_id: NodeId,
        property_name: &str,
        recurrence_threshold: f32,
        divergence_threshold: f32,
    ) -> Result<Option<DejaVuEvent>> {
        let history = self.db.get_node_history(node_id)?;

        let mut vectors: Vec<Vec<f32>> = Vec::new();
        let mut timestamps = Vec::new();

        for version in history.versions {
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    vectors.push(vec.to_vec());
                    timestamps.push(version.temporal.valid_time().start());
                }
            }
        }

        if vectors.len() < 3 {
            // Need at least 3 states: Past, Middle (diverged), Current (returned)
            return Ok(None);
        }

        let current_vec = vectors.last().unwrap();
        let current_ts = timestamps.last().unwrap();

        let prev_vec = &vectors[vectors.len() - 2];
        let recent_sim = cosine_similarity(current_vec, prev_vec)?;

        if recent_sim > divergence_threshold {
            // The node hasn't significantly changed from its immediate predecessor,
            // so this isn't a "return" from a journey, it's just staying still.
            return Ok(None);
        }

        // Search the distant past for a recurrence (we iterate backwards to find the most recent recurrence)
        for i in (0..(vectors.len() - 2)).rev() {
            let past_vec = &vectors[i];
            let sim = cosine_similarity(current_vec, past_vec)?;

            if sim >= recurrence_threshold {
                return Ok(Some(DejaVuEvent {
                    node_id,
                    current_time: *current_ts,
                    past_time: timestamps[i],
                    recurrence_similarity: sim,
                    recent_divergence: recent_sim,
                }));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_dejavu_detection() {
        let db = AletheiaDB::new().unwrap();

        // Time 1: State A
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Time 2: State B (Diverged)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // Time 3: State A again (Returned)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95, 0.0, 0.0]) // Slightly different but very similar
                    .build(),
            )
        })
        .unwrap();

        let engine = DejaVuEngine::new(&db);

        let event = engine
            .detect(node_id, "embedding", 0.9, 0.5)
            .unwrap()
            .unwrap();

        assert_eq!(event.node_id, node_id);
        assert!(event.recurrence_similarity > 0.9);
        assert!(event.recent_divergence < 0.5);
    }

    #[test]
    fn test_dejavu_no_divergence() {
        let db = AletheiaDB::new().unwrap();

        // Time 1: State A
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Time 2: State A (Still A)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // Time 3: State A (Still A)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.98, 0.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let engine = DejaVuEngine::new(&db);

        // It should be None because divergence_threshold is 0.5, but recent_sim is ~1.0
        let event = engine.detect(node_id, "embedding", 0.9, 0.5).unwrap();
        assert!(event.is_none());
    }
}
