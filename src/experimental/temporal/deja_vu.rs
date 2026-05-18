//! Deja Vu: Semantic Cycle Detection.
//!
//! "Have we been here before?"
//!
//! The Deja Vu Detector identifies nodes whose current semantic meaning (vector)
//! has returned to a state it held significantly in the past, after having drifted
//! away in the interim. This indicates cyclical behavior, regression, or recurring
//! trends in a temporal graph.
//!
//! # Concepts
//! - **Cyclical Trend**: A node moves from State A -> State B -> back to State A.
//! - **Time Gap**: The minimum time that must have passed since the node was in
//!   the previous similar state (to avoid flagging simple consecutive updates).
//! - **Deja Vu Score**: The similarity between the current state and the historical state.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::deja_vu::DejaVuDetector;
//! use aletheiadb::core::temporal::time;
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let detector = DejaVuDetector::new(&db);
//!
//! // Look for a state from at least 30 days ago that is 95% similar to now
//! let min_gap_micros = 30 * 24 * 3600 * 1_000_000;
//! let matches = detector.detect_deja_vu(node_id, "embedding", min_gap_micros, 0.95)?;
//!
//! for m in matches {
//!     println!("Deja Vu! Current state matches state from {:?} with {:.2}% similarity.",
//!         m.historical_timestamp, m.similarity * 100.0);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::cosine_similarity;

/// A detected Deja Vu event.
#[derive(Debug, Clone, PartialEq)]
pub struct DejaVuMatch {
    /// The timestamp of the historical state that matches the current state.
    pub historical_timestamp: Timestamp,
    /// The cosine similarity between the current vector and the historical vector.
    pub similarity: f32,
}

/// The Deja Vu Engine.
pub struct DejaVuDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejaVuDetector<'a> {
    /// Create a new Deja Vu Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node has returned to a previous semantic state.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `min_time_gap_micros` - Minimum microseconds that must have elapsed between
    ///   the historical state and the current state.
    /// * `min_similarity` - The minimum cosine similarity to consider it a match.
    ///
    /// # Returns
    /// A list of `DejaVuMatch` instances, ordered from most recent to oldest.
    pub fn detect_deja_vu(
        &self,
        node_id: NodeId,
        property_name: &str,
        min_time_gap_micros: i64,
        min_similarity: f32,
    ) -> Result<Vec<DejaVuMatch>> {
        let history = self.db.get_node_history(node_id)?;
        if history.versions.len() < 2 {
            return Ok(Vec::new()); // Need at least two versions to have a past
        }

        // Get the current (latest) vector
        let latest_version = history.versions.last().unwrap();
        let current_vec_opt = latest_version
            .properties
            .get(property_name)
            .and_then(|p| p.as_vector());

        let current_vec = match current_vec_opt {
            Some(v) => v.to_vec(),
            None => return Ok(Vec::new()), // Current state has no vector
        };

        let current_time = latest_version.temporal.valid_time().start().wallclock();
        let mut matches = Vec::new();

        // Iterate backwards through history, skipping the latest version
        for version in history.versions.iter().rev().skip(1) {
            let hist_time = version.temporal.valid_time().start().wallclock();

            // Check time gap
            if (current_time - hist_time) < min_time_gap_micros {
                continue;
            }

            if let Some(hist_vec) = version
                .properties
                .get(property_name)
                .and_then(|p| p.as_vector())
            {
                if current_vec.len() != hist_vec.len() {
                    continue; // Dimension mismatch, skip
                }

                // AletheiaDB vector ops are raw, so we need to copy to mutate for normalization
                let mut v_curr = current_vec.clone();
                let mut v_hist = hist_vec.to_vec();

                crate::core::vector::ops::normalize_in_place(&mut v_curr);
                crate::core::vector::ops::normalize_in_place(&mut v_hist);

                let sim = cosine_similarity(&v_curr, &v_hist)?;

                if sim >= min_similarity {
                    matches.push(DejaVuMatch {
                        historical_timestamp: version.temporal.valid_time().start(),
                        similarity: sim,
                    });
                }
            }
        }

        Ok(matches)
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    #[test]
    fn test_detect_deja_vu() {
        let db = AletheiaDB::new().unwrap();

        // State A: [1.0, 0.0]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let node_id = db.create_node("Concept", props_a).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(15));

        // State B: [0.0, 1.0] (Drifted away)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(15));

        // State C: [0.99, 0.01] (Returned to State A)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.01])
                    .build(),
            )
        })
        .unwrap();

        let detector = DejaVuDetector::new(&db);

        // Detect with a gap that includes State A but excludes State B
        // The total elapsed time between A and C is ~30ms (30,000 micros).
        // A gap of 20,000 micros will skip B (15ms ago) but include A (30ms ago).
        let matches = detector
            .detect_deja_vu(node_id, "embedding", 20_000, 0.95)
            .unwrap();

        assert_eq!(matches.len(), 1, "Should find exactly one Deja Vu match");
        assert!(
            matches[0].similarity > 0.99,
            "Similarity should be very high"
        );
    }

    #[test]
    fn test_deja_vu_too_recent() {
        let db = AletheiaDB::new().unwrap();

        // State A: [1.0, 0.0]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let node_id = db.create_node("Concept", props_a).unwrap();

        // Immediate update to similar state
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.01])
                    .build(),
            )
        })
        .unwrap();

        let detector = DejaVuDetector::new(&db);

        // Large min_gap should exclude the recent update
        let matches = detector
            .detect_deja_vu(node_id, "embedding", 10_000_000, 0.95)
            .unwrap();

        assert!(matches.is_empty(), "Should not find recent matches");
    }
}
