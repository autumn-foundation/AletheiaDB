#![allow(clippy::collapsible_if)]

//! Pulsar: Semantic Periodic Oscillation 💫
//!
//! "Does this node flip-flop between two states over time?"
//!
//! The Pulsar Engine analyzes a node's temporal history to detect if its semantic
//! vector oscillates back and forth between two distinct states (clusters).
//!
//! # Concepts
//! - **Pulsar**: A node that exhibits a repeating pattern of state A -> state B -> state A.
//! - **Oscillation Rate**: The frequency of transitions between the two dominant states.
//! - **Stability**: How cleanly the historical vectors separate into exactly two distinct states.
//!
//! # Use Cases
//! - **Indecisive Agents**: Identifying an LLM or user that rapidly switches contexts or opinions.
//! - **Seasonal Trends**: Finding concepts that shift meaning predictably over time.
//! - **Anomaly Detection**: Detecting a system that is stuck in a retry/failure loop semantically.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pulsar::PulsarEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup temporal graph ...
//! # let subject_node = NodeId::new(0).unwrap();
//!
//! let engine = PulsarEngine::new(&db);
//! // Calculate if the node is a Pulsar
//! let result = engine.calculate_pulsar(subject_node, "embedding", 0.5)?;
//!
//! if result.is_pulsar {
//!     println!("Pulsar detected! {} oscillations.", result.oscillation_count);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::{cosine_similarity, normalize_in_place};

/// The result of a Pulsar calculation.
#[derive(Debug, Clone)]
pub struct PulsarResult {
    /// True if the node exhibits significant oscillation between two states.
    pub is_pulsar: bool,
    /// The number of times the node transitioned between the two primary states.
    pub oscillation_count: usize,
    /// The centroid of the first dominant state (normalized). None if insufficient history.
    pub state_a: Option<Vec<f32>>,
    /// The centroid of the second dominant state (normalized). None if insufficient history.
    pub state_b: Option<Vec<f32>>,
    /// The semantic distance (1.0 - similarity) between State A and State B.
    pub state_distance: f32,
}

/// The Pulsar Engine.
pub struct PulsarEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> PulsarEngine<'a> {
    /// Create a new Pulsar Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate if a node is a Pulsar and return its oscillation metrics.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `distance_threshold` - The minimum distance (1.0 - cosine_similarity) between two
    ///   sequential states to count as a transition. Range [0.0, 2.0].
    pub fn calculate_pulsar(
        &self,
        node_id: NodeId,
        property_name: &str,
        distance_threshold: f32,
    ) -> Result<PulsarResult> {
        let history = self.db.get_node_history(node_id)?;

        if history.versions.len() < 3 {
            // Need at least 3 states to show A -> B -> A oscillation
            return Ok(PulsarResult {
                is_pulsar: false,
                oscillation_count: 0,
                state_a: None,
                state_b: None,
                state_distance: 0.0,
            });
        }

        // Extract vectors in chronological order
        // history.versions is usually newest first or oldest first. Let's ensure chronological
        let mut chronological_versions = history.versions.clone();
        chronological_versions.sort_by_key(|v| v.temporal.valid_time().start());

        let mut vectors: Vec<Vec<f32>> = Vec::new();
        let mut expected_dim: Option<usize> = None;

        for version in &chronological_versions {
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    let mut cloned_vec = vec.to_vec();
                    // Normalize for consistent cosine similarity distance checks
                    normalize_in_place(&mut cloned_vec);

                    match expected_dim {
                        Some(dim) => {
                            if cloned_vec.len() == dim {
                                vectors.push(cloned_vec);
                            }
                        }
                        None => {
                            if !cloned_vec.is_empty() {
                                expected_dim = Some(cloned_vec.len());
                                vectors.push(cloned_vec);
                            }
                        }
                    }
                }
            }
        }

        if vectors.len() < 3 {
            return Ok(PulsarResult {
                is_pulsar: false,
                oscillation_count: 0,
                state_a: None,
                state_b: None,
                state_distance: 0.0,
            });
        }

        // Run 2-Means clustering to find the two dominant states
        let (state_a, state_b) = Self::find_two_states(&vectors)?;

        // If the two states are too close, it's not a real oscillation, just noise
        let state_sim = cosine_similarity(&state_a, &state_b)?;
        let state_distance = (1.0 - state_sim).max(0.0);

        if state_distance < distance_threshold {
            return Ok(PulsarResult {
                is_pulsar: false,
                oscillation_count: 0,
                state_a: Some(state_a),
                state_b: Some(state_b),
                state_distance,
            });
        }

        // Count transitions
        let mut oscillation_count = 0;
        let mut current_state = Self::assign_to_state(&vectors[0], &state_a, &state_b)?;

        for v in vectors.iter().skip(1) {
            let next_state = Self::assign_to_state(v, &state_a, &state_b)?;
            if next_state != current_state {
                oscillation_count += 1;
                current_state = next_state;
            }
        }

        // We require at least 2 transitions to be a pulsar (e.g., A -> B -> A)
        let is_pulsar = oscillation_count >= 2;

        Ok(PulsarResult {
            is_pulsar,
            oscillation_count,
            state_a: Some(state_a),
            state_b: Some(state_b),
            state_distance,
        })
    }

    /// Helper to find 2 distinct centroids using a simplified k-means
    fn find_two_states(vectors: &[Vec<f32>]) -> Result<(Vec<f32>, Vec<f32>)> {
        let dim = vectors[0].len();

        // Initialize centroids deterministically
        // C1 = first vector
        let mut c1 = vectors[0].clone();

        // C2 = vector furthest from C1
        let mut c2 = vectors[0].clone();
        let mut max_dist = -1.0;
        for v in vectors {
            let dist = 1.0 - cosine_similarity(&c1, v).unwrap_or(1.0);
            if dist > max_dist {
                max_dist = dist;
                c2 = v.clone();
            }
        }

        // If max_dist is 0, all vectors are identical
        if max_dist <= 0.0 {
            return Ok((c1.clone(), c2.clone()));
        }

        let max_iters = 10;
        let mut assignments = vec![0; vectors.len()];

        for _ in 0..max_iters {
            let mut changes = 0;
            let mut sum1 = vec![0.0; dim];
            let mut sum2 = vec![0.0; dim];
            let mut count1 = 0;
            let mut count2 = 0;

            // Assign
            for (i, v) in vectors.iter().enumerate() {
                let dist1 = 1.0 - cosine_similarity(v, &c1).unwrap_or(1.0);
                let dist2 = 1.0 - cosine_similarity(v, &c2).unwrap_or(1.0);

                let best_cluster = if dist1 < dist2 { 0 } else { 1 };

                if assignments[i] != best_cluster {
                    assignments[i] = best_cluster;
                    changes += 1;
                }

                if best_cluster == 0 {
                    for d in 0..dim {
                        sum1[d] += v[d];
                    }
                    count1 += 1;
                } else {
                    for d in 0..dim {
                        sum2[d] += v[d];
                    }
                    count2 += 1;
                }
            }

            // Update
            if count1 > 0 {
                for d in 0..dim {
                    c1[d] = sum1[d] / count1 as f32;
                }
                normalize_in_place(&mut c1);
            }
            if count2 > 0 {
                for d in 0..dim {
                    c2[d] = sum2[d] / count2 as f32;
                }
                normalize_in_place(&mut c2);
            }

            if changes == 0 {
                break;
            }
        }

        Ok((c1, c2))
    }

    /// Assigns a vector to state A (0) or state B (1)
    fn assign_to_state(v: &[f32], state_a: &[f32], state_b: &[f32]) -> Result<usize> {
        let sim_a = cosine_similarity(v, state_a)?;
        let sim_b = cosine_similarity(v, state_b)?;
        if sim_a > sim_b { Ok(0) } else { Ok(1) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_pulsar_detected() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();

        // State 1: A [1.0, 0.0]
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Bot",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // State 2: B [0.0, 1.0]
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // State 3: A [1.0, 0.0]  (Transition 2)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // State 4: B [0.0, 1.0]  (Transition 3)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let engine = PulsarEngine::new(&db);
        let result = engine.calculate_pulsar(n1, "vec", 0.5).unwrap();

        assert!(result.is_pulsar);
        assert_eq!(result.oscillation_count, 3);
        assert!(result.state_distance > 0.5);
    }

    #[test]
    fn test_pulsar_not_oscillating() {
        let db = AletheiaDB::new().unwrap();
        let mut n1 = NodeId::new(0).unwrap();

        // State 1: A
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Bot",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // State 2: A slightly shifted
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // State 3: A slightly shifted
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.98, 0.02])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let engine = PulsarEngine::new(&db);
        let result = engine.calculate_pulsar(n1, "vec", 0.5).unwrap();

        // Distance between the two pseudo-clusters will be very small
        assert!(!result.is_pulsar);
        assert_eq!(result.oscillation_count, 0);
        assert!(result.state_distance < 0.5);
    }
}
