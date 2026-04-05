//! Pendulum: Semantic Oscillation Detector 🕰️
//!
//! "Is this concept evolving, or just flip-flopping?"
//!
//! Pendulum analyzes a node's historical vector embeddings to detect semantic oscillation.
//! Oscillation occurs when a node's meaning repeatedly bounces back and forth between
//! distinct states over time, rather than progressing in a linear or random fashion.
//!
//! # The Hook
//!
//! In temporal graphs, a concept that changes isn't necessarily bad. But a concept that
//! rapidly flip-flops between "State A" and "State B" often indicates instability,
//! automated bot behavior (e.g., reverting an edit war), or a highly contested entity.
//! Pendulum measures this "semantic whiplash".
//!
//! # How it works
//!
//! For a given node and property:
//! 1. Retrieves the chronological history of the vector property.
//! 2. Calculates the delta vectors between consecutive versions: `D_i = V_i - V_{i-1}`.
//! 3. Calculates the cosine similarity between consecutive deltas: `sim(D_i, D_{i+1})`.
//! 4. If the deltas are consistently pointing in opposite directions (cosine similarity near -1),
//!    the node is oscillating.
//!
//! # Example
//!
//! ```rust
//! // Requires features = ["nova"]
//! # #[cfg(feature = "nova")] {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pendulum::Pendulum;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... create node and update it back and forth ...
//!
//! let pendulum = Pendulum::new(&db);
//! let result = pendulum.analyze_oscillation(/* node_id */ 0.into(), "embedding")?;
//!
//! if result.oscillation_score > 0.8 {
//!     println!("⚠️ Warning: Node is experiencing severe semantic oscillation!");
//! }
//! # Ok(())
//! # }
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// The result of a Pendulum oscillation analysis.
#[derive(Debug, Clone)]
pub struct OscillationResult {
    /// The number of valid vector updates found in the node's history.
    pub update_count: usize,
    /// The oscillation score, from 0.0 (no oscillation/linear) to 1.0 (perfect back-and-forth).
    pub oscillation_score: f32,
    /// The average magnitude of the shifts. If oscillation is high but magnitude is tiny, it might just be noise.
    pub average_shift_magnitude: f32,
}

/// The Pendulum Engine.
pub struct Pendulum<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Pendulum<'a> {
    /// Create a new Pendulum instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a node's historical vectors for oscillation.
    pub fn analyze_oscillation(
        &self,
        node_id: NodeId,
        property: &str,
    ) -> Result<OscillationResult> {
        // 1. Get the history of the node
        let history = self.db.get_node_history(node_id)?;

        // 2. Extract vectors in chronological order
        // Note: history.versions are typically sorted by valid_time or tx_time.
        // Assuming they are sorted chronologically by when the version was valid.
        let mut vectors = Vec::new();

        // Sort versions by valid time start just to be sure
        let mut versions = history.versions;
        versions.sort_by_key(|v| v.temporal.valid_time().start().wallclock());

        for version in versions {
            #[allow(clippy::collapsible_if)]
            if let Some(val) = version.properties.get(property) {
                if let Some(vec) = val.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        if vectors.len() < 3 {
            // Need at least 3 points to have 2 deltas to compare
            return Ok(OscillationResult {
                update_count: vectors.len(),
                oscillation_score: 0.0,
                average_shift_magnitude: 0.0,
            });
        }

        // 3. Calculate Deltas
        let mut deltas = Vec::new();
        let mut total_magnitude = 0.0;

        for i in 1..vectors.len() {
            let prev = &vectors[i - 1];
            let curr = &vectors[i];

            if prev.len() != curr.len() {
                return Err(Error::other(format!(
                    "Vector dimension mismatch at history step {}: {} vs {}",
                    i,
                    prev.len(),
                    curr.len()
                )));
            }

            let mut delta = vec![0.0; prev.len()];
            let mut mag_sq = 0.0;
            for j in 0..prev.len() {
                let diff = curr[j] - prev[j];
                delta[j] = diff;
                mag_sq += diff * diff;
            }

            let mag = mag_sq.sqrt();
            total_magnitude += mag;

            // Normalize delta to isolate direction
            if mag > f32::EPSILON {
                for val in &mut delta {
                    *val /= mag;
                }
            }

            deltas.push((delta, mag));
        }

        let average_shift_magnitude = total_magnitude / deltas.len() as f32;

        // 4. Compare consecutive deltas
        // We look at the cosine similarity of D_i and D_{i+1}.
        // If a node goes A -> B -> A, the delta D1 is B-A, D2 is A-B.
        // The cosine similarity between D1 and D2 will be -1.0.
        // We map similarity from [-1.0, 1.0] to an "oscillation factor" [1.0, 0.0].
        // Specifically, oscillation_factor = (1.0 - similarity) / 2.0.
        // -1.0 (perfect reversal) -> 1.0
        // 0.0 (orthogonal) -> 0.5
        // 1.0 (same direction) -> 0.0

        let mut total_oscillation = 0.0;
        let mut comparison_count = 0;

        for i in 1..deltas.len() {
            let (d1, mag1) = &deltas[i - 1];
            let (d2, mag2) = &deltas[i];

            // Ignore tiny shifts that are just floating point noise
            if *mag1 > f32::EPSILON && *mag2 > f32::EPSILON {
                // Since d1 and d2 are normalized, dot product IS cosine similarity
                let mut similarity = 0.0;
                for j in 0..d1.len() {
                    similarity += d1[j] * d2[j];
                }

                // Clamp for safety
                similarity = similarity.clamp(-1.0, 1.0);

                let oscillation_factor = (1.0 - similarity) / 2.0;
                total_oscillation += oscillation_factor;
                comparison_count += 1;
            }
        }

        let oscillation_score = if comparison_count > 0 {
            total_oscillation / comparison_count as f32
        } else {
            0.0
        };

        Ok(OscillationResult {
            update_count: vectors.len(),
            oscillation_score,
            average_shift_magnitude,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use std::time::Duration;

    #[test]
    fn test_pendulum_perfect_oscillation() {
        let db = AletheiaDB::new().unwrap();

        let v_a = vec![1.0, 0.0];
        let v_b = vec![0.0, 1.0];

        // Create node at state A
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new().insert_vector("vec", &v_a).build(),
                )
            })
            .unwrap();

        // Update B
        std::thread::sleep(Duration::from_millis(5));
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new().insert_vector("vec", &v_b).build(),
            )
        })
        .unwrap();

        // Update A
        std::thread::sleep(Duration::from_millis(5));
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new().insert_vector("vec", &v_a).build(),
            )
        })
        .unwrap();

        // Update B
        std::thread::sleep(Duration::from_millis(5));
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new().insert_vector("vec", &v_b).build(),
            )
        })
        .unwrap();

        let pendulum = Pendulum::new(&db);
        let result = pendulum.analyze_oscillation(node_id, "vec").unwrap();

        assert_eq!(result.update_count, 4);
        // Should be very close to 1.0 because it's a perfect back-and-forth
        assert!(result.oscillation_score > 0.99);
        assert!(result.average_shift_magnitude > 0.0);
    }

    #[test]
    fn test_pendulum_linear_progression() {
        let db = AletheiaDB::new().unwrap();

        // A -> B -> C -> D (all moving in same +X direction)
        let vectors = [
            vec![0.0, 0.0],
            vec![1.0, 0.0],
            vec![2.0, 0.0],
            vec![3.0, 0.0],
        ];

        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &vectors[0])
                        .build(),
                )
            })
            .unwrap();

        for vec in vectors.iter().skip(1) {
            std::thread::sleep(Duration::from_millis(5));
            db.write(|tx| {
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new().insert_vector("vec", vec).build(),
                )
            })
            .unwrap();
        }

        let pendulum = Pendulum::new(&db);
        let result = pendulum.analyze_oscillation(node_id, "vec").unwrap();

        assert_eq!(result.update_count, 4);
        // Linear movement: sim = 1.0 -> score = (1.0 - 1.0)/2 = 0.0
        assert!(result.oscillation_score < 0.01);
    }

    #[test]
    fn test_pendulum_orthogonal_drift() {
        let db = AletheiaDB::new().unwrap();

        // Moving in a square (orthogonal changes)
        let vectors = [
            vec![0.0, 0.0],
            vec![1.0, 0.0], // Delta [1,0]
            vec![1.0, 1.0], // Delta [0,1]
            vec![0.0, 1.0], // Delta [-1,0]
        ];

        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &vectors[0])
                        .build(),
                )
            })
            .unwrap();

        for vec in vectors.iter().skip(1) {
            std::thread::sleep(Duration::from_millis(5));
            db.write(|tx| {
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new().insert_vector("vec", vec).build(),
                )
            })
            .unwrap();
        }

        let pendulum = Pendulum::new(&db);
        let result = pendulum.analyze_oscillation(node_id, "vec").unwrap();

        // Orthogonal moves: sim = 0.0 -> score = (1.0 - 0.0)/2 = 0.5
        assert!((result.oscillation_score - 0.5).abs() < 0.01);
    }
}
