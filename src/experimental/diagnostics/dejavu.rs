//! DejaVu: Cyclic Semantic Regression Detector.
//!
//! "We've been here before."
//!
//! The DejaVu Detector identifies entities that experience "semantic regression" over time.
//! That is, an entity starts with a specific semantic meaning (Vector A), drifts significantly
//! to a new meaning (Vector B), and then reverts back to a meaning very close to the original (Vector A').
//!
//! # Use Cases
//! - **Flip-Flopping**: Detecting LLM agents or users that oscillate between two contrasting concepts.
//! - **Failed Experiments**: An entity tried to evolve into a new category but retreated to its baseline.
//! - **Cyclic Behavior**: Identifying recurring patterns in semantic states over long periods.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::dejavu::DejaVuDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = aletheiadb::core::id::NodeId::new(0).unwrap();
//! let detector = DejaVuDetector::new(&db);
//!
//! // divergence_threshold: How far must it drift to be considered a "departure"? (e.g. cosine dist > 0.5)
//! // return_threshold: How close must it get to the original to be a "return"? (e.g. cosine sim > 0.9)
//! let cycles = detector.detect_cycles(node_id, "embedding", 0.5, 0.9)?;
//!
//! for cycle in cycles {
//!     println!("Deja Vu Event! Node {} drifted away at version {} and returned at version {}.",
//!         node_id, cycle.divergence_version, cycle.return_version);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// A detected cyclic semantic regression event.
#[derive(Debug, Clone, PartialEq)]
pub struct DejaVuEvent {
    /// The original version number that established the baseline.
    pub origin_version: u64,
    /// The version number where the semantic meaning significantly diverged.
    pub divergence_version: u64,
    /// The version number where the semantic meaning returned to the baseline.
    pub return_version: u64,
    /// The cosine distance during the divergence phase (1.0 - similarity).
    pub divergence_distance: f32,
    /// The cosine similarity upon return to the baseline.
    pub return_similarity: f32,
}

/// The DejaVu Detector Engine.
pub struct DejaVuDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejaVuDetector<'a> {
    /// Create a new DejaVuDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect semantic cycles (Origin -> Divergence -> Return) for a specific node.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to track.
    /// * `divergence_threshold` - Minimum cosine distance (1.0 - similarity) required to count as a "departure" from the origin.
    /// * `return_threshold` - Minimum cosine similarity required to count as a "return" to the origin.
    pub fn detect_cycles(
        &self,
        node_id: NodeId,
        property_name: &str,
        divergence_threshold: f32,
        return_threshold: f32,
    ) -> Result<Vec<DejaVuEvent>> {
        let history = self.db.get_node_history(node_id)?;
        let mut events = Vec::new();

        if history.versions.len() < 3 {
            return Ok(events); // Need at least Origin, Divergence, and Return
        }

        // We will maintain a list of 'baselines' (origins). Every version is a potential baseline.
        // For each baseline, we scan forward to find if it diverges and then returns.

        // Extract all vectors and version numbers first to simplify logic
        let mut temporal_states = Vec::new();
        for version in &history.versions {
            if let Some(prop) = version.properties.get(property_name)
                && let Some(vec) = prop.as_vector()
            {
                temporal_states.push((version.version_number, vec.to_vec()));
            }
        }
        if temporal_states.len() < 3 {
            return Ok(events);
        }

        for i in 0..temporal_states.len() {
            let (origin_version, origin_vec) = &temporal_states[i];

            let mut divergence_detected = false;
            let mut divergence_version = 0;
            let mut max_divergence_dist = 0.0;

            // Scan forward from origin
            for (current_version, current_vec) in temporal_states.iter().skip(i + 1) {
                let similarity = match cosine_similarity(origin_vec, current_vec) {
                    Ok(sim) => sim,
                    Err(_) => continue, // Dimension mismatch or zero vector
                };

                let distance = 1.0 - similarity;

                if !divergence_detected {
                    // Looking for divergence phase
                    if distance >= divergence_threshold {
                        divergence_detected = true;
                        divergence_version = *current_version;
                        max_divergence_dist = distance;
                    }
                } else {
                    // We are in divergence phase, looking for return phase
                    // Update max divergence just in case it keeps drifting before returning
                    if distance > max_divergence_dist {
                        max_divergence_dist = distance;
                        divergence_version = *current_version; // Re-anchor the peak divergence
                    }

                    if similarity >= return_threshold {
                        // Return detected!
                        events.push(DejaVuEvent {
                            origin_version: *origin_version,
                            divergence_version,
                            return_version: *current_version,
                            divergence_distance: max_divergence_dist,
                            return_similarity: similarity,
                        });

                        // Break out of the j-loop because we found a complete cycle for this origin.
                        // (If we want to find multiple cycles from the same origin, we'd reset state instead of breaking,
                        // but usually a single return completes the "deja vu" arc for that baseline).
                        break;
                    }
                }
            }
        }

        // It is possible for the same return event to match multiple origins that were similar to each other.
        // For simplicity, we return all detected cycles.
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_deja_vu_detection() {
        let db = AletheiaDB::new().unwrap();

        // Node creation (Origin)
        let mut node_id = NodeId::new(0).unwrap();
        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Divergence
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Return
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let detector = DejaVuDetector::new(&db);

        // threshold 0.5 for div (cos sim 0.0 -> dist 1.0)
        // threshold 0.9 for ret
        let cycles = detector.detect_cycles(node_id, "vec", 0.5, 0.9).unwrap();

        assert_eq!(cycles.len(), 1);
        let cycle = &cycles[0];

        assert_eq!(cycle.origin_version, 1);
        assert_eq!(cycle.divergence_version, 2);
        assert_eq!(cycle.return_version, 3);
        assert!(cycle.divergence_distance > 0.9); // dist( [1,0], [0,1] ) is 1.0
        assert!(cycle.return_similarity > 0.9);
    }

    #[test]
    fn test_deja_vu_no_return() {
        let db = AletheiaDB::new().unwrap();

        // Node creation (Origin)
        let mut node_id = NodeId::new(0).unwrap();
        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Divergence 1
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Divergence 2 (no return)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let detector = DejaVuDetector::new(&db);
        let cycles = detector.detect_cycles(node_id, "vec", 0.5, 0.9).unwrap();

        assert_eq!(cycles.len(), 0);
    }
}
