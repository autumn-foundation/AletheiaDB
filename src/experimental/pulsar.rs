#![allow(clippy::collapsible_if)]

//! Pulsar: Temporal Vector Oscillation Detector 💫
//!
//! "Does this node constantly change its mind?"
//!
//! Pulsar analyzes the temporal history of a node's vector property
//! to detect if it oscillates back and forth between different semantic states.
//!
//! # Concepts
//! - **Oscillation Score**: Measures how often consecutive vector movements
//!   reverse direction. A high score means the vector is moving back and forth
//!   (e.g., an article being repeatedly edited back to a previous viewpoint).
//! - **Temporal Trajectory**: The path a vector takes over time.
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
//! // ... setup graph with historical versions ...
//! # let node_id = NodeId::new(1).unwrap();
//!
//! let pulsar = Pulsar::new(&db);
//! let score = pulsar.analyze_oscillation(node_id, "embedding")?;
//!
//! println!("Oscillation Score: {}", score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// The Pulsar Engine.
pub struct Pulsar<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Pulsar<'a> {
    /// Create a new Pulsar Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the oscillation score of a node's vector over time.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to analyze.
    /// * `property_name` - The vector property to track.
    ///
    /// # Returns
    /// An oscillation score from `0.0` (no oscillation, moving straight or stationary)
    /// up to `1.0` (perfectly oscillating back and forth).
    pub fn analyze_oscillation(&self, node_id: NodeId, property_name: &str) -> Result<f32> {
        let mut vectors = Vec::new();

        // Get all historical versions of the node directly from the database API
        let history = self.db.get_node_history(node_id)?;
        for version in history.versions {
            if let Some(prop) = version
                .properties
                .get(property_name)
                .and_then(|p| p.as_vector())
            {
                vectors.push(prop.to_vec());
            }
        }

        // Sort vectors chronologically (they are usually returned newest-first or oldest-first, let's assume we want them in order of occurrence)
        // Actually, get_node_history returns newest first usually. Let's reverse to get chronological.
        vectors.reverse();

        if vectors.len() < 3 {
            // Need at least 3 points to have 2 movements to compare
            return Ok(0.0);
        }

        let mut oscillation_score = 0.0;
        let mut movement_count = 0;

        for i in 0..(vectors.len() - 2) {
            let v1 = &vectors[i];
            let v2 = &vectors[i + 1];
            let v3 = &vectors[i + 2];

            // Ensure vector dimensions match to avoid panics
            if v1.len() != v2.len() || v2.len() != v3.len() {
                continue;
            }

            // Calculate movement vectors: m1 = v2 - v1, m2 = v3 - v2
            let mut m1 = vec![0.0_f32; v1.len()];
            let mut m2 = vec![0.0_f32; v1.len()];

            let mut m1_zero = true;
            let mut m2_zero = true;

            for j in 0..v1.len() {
                m1[j] = v2[j] - v1[j];
                m2[j] = v3[j] - v2[j];

                if m1[j].abs() > 1e-6_f32 {
                    m1_zero = false;
                }
                if m2[j].abs() > 1e-6_f32 {
                    m2_zero = false;
                }
            }

            // If either movement is zero, there's no direction to reverse
            if m1_zero || m2_zero {
                continue;
            }

            // Normalize movement vectors to compare their directions
            ops::normalize_in_place(&mut m1);
            ops::normalize_in_place(&mut m2);

            // Dot product of normalized movements = cosine of angle between them
            // +1.0 = same direction
            //  0.0 = orthogonal
            // -1.0 = opposite direction (perfect oscillation)
            let similarity = ops::dot_product(&m1, &m2)?;

            // Map [-1.0, 1.0] to [1.0, 0.0] where -1.0 (opposite) gives 1.0 (high oscillation)
            let reversal = (1.0 - similarity) / 2.0;

            oscillation_score += reversal;
            movement_count += 1;
        }

        if movement_count == 0 {
            return Ok(0.0);
        }

        Ok(oscillation_score / (movement_count as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::error::Error;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_pulsar_no_oscillation() {
        let db = AletheiaDB::new().unwrap();
        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        // Move linearly
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
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
                    .insert_vector("embedding", &[2.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let pulsar = Pulsar::new(&db);
        let score = pulsar.analyze_oscillation(node_id, "embedding").unwrap();

        // Straight line, score should be 0.0
        assert!(score < 0.01);
    }

    #[test]
    fn test_pulsar_perfect_oscillation() {
        let db = AletheiaDB::new().unwrap();
        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        // Move to A
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        // Move back to origin
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let pulsar = Pulsar::new(&db);
        let score = pulsar.analyze_oscillation(node_id, "embedding").unwrap();

        // Complete reversal, score should be ~1.0
        assert!(score > 0.99);
    }
}
