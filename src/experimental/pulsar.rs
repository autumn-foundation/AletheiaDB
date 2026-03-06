#![allow(clippy::collapsible_if)]

//! Pulsar: Semantic Oscillation Detector 💫
//!
//! "Is this concept evolving or just spinning in circles?"
//!
//! Pulsar analyzes the temporal trajectory of a node's vector embedding
//! to detect cyclic behavior or oscillation. If a node repeatedly shifts
//! back and forth between two or more semantic states over time, it's a Pulsar.
//!
//! # Concepts
//! - **Trajectory**: The sequence of a node's vectors across its history.
//! - **Oscillation Score**: A measure of how much the node's vector sequence
//!   revisits previous states instead of progressing linearly or drifting randomly.
//!   High score means high cyclic behavior.
//!
//! # Use Cases
//! - **Flip-Flop Detection**: Identifying agents or users who constantly revert
//!   their opinions or preferences.
//! - **A/B Testing Loops**: Detecting entities caught in continuous state toggles.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pulsar::Pulsar;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph and node history ...
//! # let node_id = NodeId::new(0).unwrap();
//!
//! let pulsar = Pulsar::new(&db);
//! let score = pulsar.detect_oscillation(node_id, "embedding")?;
//!
//! println!("Oscillation Score: {:.2}", score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// The Pulsar Engine for detecting semantic oscillation.
pub struct Pulsar<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Pulsar<'a> {
    /// Create a new Pulsar Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect the oscillation score of a node's vector history.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property tracking the semantic state.
    ///
    /// # Returns
    /// An oscillation score from `0.0` to `1.0`.
    /// - `1.0` means perfect back-and-forth oscillation (A -> B -> A).
    /// - `0.0` means linear progression or random walk with no return.
    pub fn detect_oscillation(&self, node_id: NodeId, property_name: &str) -> Result<f32> {
        let history = self.db.get_node_history(node_id)?;

        // Extract vectors chronologically
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for version in history.versions {
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        if vectors.len() < 3 {
            // Can't oscillate with less than 3 states
            return Ok(0.0);
        }

        let mut oscillation_sum = 0.0;
        let mut triplets_evaluated = 0;

        // Look at rolling triplets: A -> B -> C
        // Oscillation occurs when A -> B has distance, B -> C has distance,
        // but A -> C is very similar (meaning it returned to the old state).
        for i in 0..vectors.len() - 2 {
            let vec_a = &vectors[i];
            let vec_b = &vectors[i + 1];
            let vec_c = &vectors[i + 2];

            if vec_a.len() != vec_b.len() || vec_b.len() != vec_c.len() {
                continue; // Dimension mismatch, skip
            }

            // Normalize vectors for cosine similarity
            let mut norm_a = vec_a.clone();
            let mut norm_b = vec_b.clone();
            let mut norm_c = vec_c.clone();

            ops::normalize_in_place(&mut norm_a);
            ops::normalize_in_place(&mut norm_b);
            ops::normalize_in_place(&mut norm_c);

            let sim_ab = ops::cosine_similarity(&norm_a, &norm_b)?;
            let sim_bc = ops::cosine_similarity(&norm_b, &norm_c)?;
            let sim_ac = ops::cosine_similarity(&norm_a, &norm_c)?;

            // Distance is 1.0 - similarity. High distance means it moved.
            let dist_ab = (1.0 - sim_ab).max(0.0);
            let dist_bc = (1.0 - sim_bc).max(0.0);

            // Reversal is high when A and C are very similar (sim_ac is high)
            // AND the node actually moved in between (dist_ab and dist_bc are high).

            // Base reversal strength (how similar A and C are)
            let reversal_strength = sim_ac.max(0.0);

            // Movement strength (did it actually go somewhere and come back?)
            let movement_strength = (dist_ab * dist_bc).sqrt();

            // Multiply them. We scale up movement strength since cosine distance
            // of orthogonal vectors is 1.0, so movement_strength is usually < 1.0.
            let triplet_score = (reversal_strength * movement_strength * 2.0).clamp(0.0, 1.0);

            oscillation_sum += triplet_score;
            triplets_evaluated += 1;
        }

        if triplets_evaluated == 0 {
            return Ok(0.0);
        }

        Ok(oscillation_sum / triplets_evaluated as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_perfect_oscillation() {
        let db = AletheiaDB::new().unwrap();

        let state_a = vec![1.0, 0.0];
        let state_b = vec![0.0, 1.0];

        // Version 1: State A
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &state_a)
                        .build(),
                )
            })
            .unwrap();

        // Version 2: State B
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("opinion", &state_b)
                    .build(),
            )
        })
        .unwrap();

        // Version 3: Back to State A
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("opinion", &state_a)
                    .build(),
            )
        })
        .unwrap();

        // Version 4: Back to State B
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("opinion", &state_b)
                    .build(),
            )
        })
        .unwrap();

        let pulsar = Pulsar::new(&db);
        let score = pulsar.detect_oscillation(node_id, "opinion").unwrap();

        // It should detect the A -> B -> A -> B pattern
        assert!(
            score > 0.8,
            "Expected high oscillation score, got {}",
            score
        );
    }

    #[test]
    fn test_linear_progression() {
        let db = AletheiaDB::new().unwrap();

        // Version 1: State 1
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[1.0, 0.0, 0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Version 2: State 2
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("opinion", &[0.0, 1.0, 0.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // Version 3: State 3
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("opinion", &[0.0, 0.0, 1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // Version 4: State 4
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("opinion", &[0.0, 0.0, 0.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        let pulsar = Pulsar::new(&db);
        let score = pulsar.detect_oscillation(node_id, "opinion").unwrap();

        // Linear movement without returning to previous states should have low score
        assert!(score < 0.2, "Expected low oscillation score, got {}", score);
    }
}
