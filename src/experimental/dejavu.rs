#![allow(clippy::collapsible_if)]

//! DejaVu: Semantic Recurrence Detector 🔄
//!
//! "History doesn't repeat itself, but it often rhymes."
//!
//! The DejaVu Engine identifies semantic recurrences—situations where an entity's
//! vector drifts away from a past state and later returns to it.
//!
//! # Concepts
//! - **Semantic Recurrence**: An entity's vector at $T_n$ is highly similar to its vector
//!   at $T_0$, but its vector at some intermediate time $T_i$ was significantly different.
//! - **Recurrence Score**: A measure of how strongly an entity has "returned" to a past state
//!   after a meaningful deviation. High score = strong return.
//!
//! # Use Cases
//! - **Behavioral Loops**: Detecting users who repeatedly fall back into old habits.
//! - **Narrative Arcs**: Finding characters or concepts that complete a "hero's journey"
//!   by returning to their origin, changed but semantically similar.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::dejavu::DejaVuDetector;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup historical node ...
//! # let node_id = NodeId::new(0).unwrap();
//!
//! let detector = DejaVuDetector::new(&db);
//! let score = detector.detect_recurrence(node_id, "embedding")?;
//!
//! println!("DejaVu Score: {}", score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// Result of a DejaVu recurrence analysis.
#[derive(Debug, Clone)]
pub struct DejaVuResult {
    /// The maximum divergence observed during the timeline (0.0 to 1.0).
    pub max_divergence: f32,
    /// The final similarity back to the original state (0.0 to 1.0).
    pub return_similarity: f32,
    /// The overall recurrence score. High score indicates a strong deviation followed by a strong return.
    pub recurrence_score: f32,
}

/// The DejaVu Engine.
pub struct DejaVuDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejaVuDetector<'a> {
    /// Create a new DejaVu Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the recurrence score for a node's historical vector property.
    ///
    /// The analysis looks at the node's history and compares each version's vector
    /// to the earliest known vector (baseline). It finds the point of maximum deviation
    /// and then checks if the final vector has returned to be similar to the baseline.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    pub fn detect_recurrence(&self, node_id: NodeId, property_name: &str) -> Result<f32> {
        let result = self.analyze(node_id, property_name)?;
        Ok(result.recurrence_score)
    }

    /// Perform a full recurrence analysis.
    pub fn analyze(&self, node_id: NodeId, property_name: &str) -> Result<DejaVuResult> {
        let history = self.db.get_node_history(node_id)?;

        // Extract valid vectors from the history, chronologically
        let mut vectors = Vec::new();
        for version in history.versions.iter() {
            if let Some(prop) = version
                .properties
                .get(property_name)
                .and_then(|p| p.as_vector())
            {
                let mut vec = prop.to_vec();
                ops::normalize_in_place(&mut vec);
                vectors.push(vec);
            }
        }

        if vectors.len() < 3 {
            // Need at least 3 points: Start, Deviation, Return
            return Ok(DejaVuResult {
                max_divergence: 0.0,
                return_similarity: 1.0,
                recurrence_score: 0.0,
            });
        }

        let baseline = &vectors[0];
        let final_vec = &vectors[vectors.len() - 1];

        // Find the maximum deviation from the baseline
        let mut max_divergence = 0.0_f32;

        for vec in vectors.iter().skip(1).take(vectors.len() - 2) {
            let sim = ops::cosine_similarity(baseline, vec)?;
            let div = (1.0 - sim).max(0.0);
            if div > max_divergence {
                max_divergence = div;
            }
        }

        // Calculate how similar the final vector is to the baseline
        let return_similarity = ops::cosine_similarity(baseline, final_vec)?.max(0.0);

        // Recurrence score combines high divergence with high return similarity.
        // If it didn't diverge much, it's not a recurrence, just stasis.
        // If it didn't return, it's just drift.
        // Example: Divergence of 0.8, Return Similarity of 0.9 -> Score of 0.72
        let recurrence_score = max_divergence * return_similarity;

        Ok(DejaVuResult {
            max_divergence,
            return_similarity,
            recurrence_score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::error::Error;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_dejavu_recurrence() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(0).unwrap();

        // 1. Initial State (Baseline)
        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        // 2. Deviation State (Moves away from baseline)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        // 3. Return State (Returns to something very similar to baseline)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.1])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = DejaVuDetector::new(&db);
        let result = detector.analyze(node_id, "embedding").unwrap();

        assert!(result.max_divergence > 0.5); // It diverged significantly
        assert!(result.return_similarity > 0.9); // It returned significantly
        assert!(result.recurrence_score > 0.5); // Overall high recurrence
    }

    #[test]
    fn test_dejavu_no_recurrence_just_drift() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = DejaVuDetector::new(&db);
        let result = detector.analyze(node_id, "embedding").unwrap();

        // It diverged, but never returned
        assert!(result.max_divergence > 0.5);
        assert!(result.return_similarity < 0.1);
        assert!(result.recurrence_score < 0.1);
    }
}
