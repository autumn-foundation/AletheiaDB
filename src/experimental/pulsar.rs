#![allow(clippy::collapsible_if)]

//! Pulsar: Semantic Rhythm & Periodicity Detector.
//!
//! "Does this node have a heartbeat?"
//!
//! Pulsar detects nodes that oscillate between two or more distinct semantic states
//! over time. Instead of just tracking drift (like `Thermos`), Pulsar looks for
//! return trips.
//!
//! # Concepts
//! - **Oscillation**: A pattern where a node moves from State A to State B,
//!   and then back to State A.
//! - **Pulsar Score**: A measure of how strongly a node's semantic history
//!   exhibits periodic return behavior.
//!
//! # Use Cases
//! - **Behavioral Rhythms**: A user who works during the day (Code/Emails)
//!   and games at night (Gaming/Media).
//! - **Seasonal Trends**: A company whose quarterly reports cycle between
//!   "Planning" and "Execution" semantics.
//! - **Bot Detection**: Accounts that switch contexts too mechanically.
//!
//! # Example
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pulsar::Pulsar;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;
//!
//! let pulsar = Pulsar::new(&db);
//!
//! // Detect if the node oscillates back and forth
//! let result = pulsar.detect_rhythm(node_id, "embedding", 3)?;
//!
//! if result.is_pulsar {
//!     println!("Node exhibits rhythmic semantic shifts! Score: {:.2}", result.score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The result of a Pulsar rhythm analysis.
#[derive(Debug, Clone)]
pub struct PulsarResult {
    /// True if the node exhibits strong periodic return behavior.
    pub is_pulsar: bool,
    /// The strength of the rhythm (0.0 to 1.0).
    /// Higher means tighter alternating clusters.
    pub score: f32,
    /// The number of versions analyzed.
    pub history_len: usize,
}

/// The Pulsar Engine.
pub struct Pulsar<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Pulsar<'a> {
    /// Create a new Pulsar engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node's history oscillates.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `min_versions` - Minimum number of historical versions required to analyze (e.g. 3).
    pub fn detect_rhythm(
        &self,
        node_id: NodeId,
        property_name: &str,
        min_versions: usize,
    ) -> Result<PulsarResult> {
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

        if vectors.len() < min_versions {
            return Ok(PulsarResult {
                is_pulsar: false,
                score: 0.0,
                history_len: vectors.len(),
            });
        }

        // Calculate the score.
        // We look for A -> B -> A -> B patterns.
        // Thus, consecutive versions (v_i, v_{i+1}) should have lower similarity than alternating versions (v_i, v_{i+2}).

        let mut consecutive_sim_sum = 0.0_f32;
        let mut consecutive_count = 0;

        for i in 0..vectors.len() - 1 {
            if let Ok(sim) = cosine_similarity(&vectors[i], &vectors[i + 1]) {
                consecutive_sim_sum += sim;
                consecutive_count += 1;
            }
        }

        let mut alternating_sim_sum = 0.0_f32;
        let mut alternating_count = 0;

        for i in 0..vectors.len() - 2 {
            if let Ok(sim) = cosine_similarity(&vectors[i], &vectors[i + 2]) {
                alternating_sim_sum += sim;
                alternating_count += 1;
            }
        }

        if consecutive_count == 0 || alternating_count == 0 {
            return Ok(PulsarResult {
                is_pulsar: false,
                score: 0.0,
                history_len: vectors.len(),
            });
        }

        let avg_consecutive = consecutive_sim_sum / (consecutive_count as f32);
        let avg_alternating = alternating_sim_sum / (alternating_count as f32);

        // Rhythm score is high if alternating similarity is much higher than consecutive similarity.
        let mut score = avg_alternating - avg_consecutive;

        // Normalize the score conceptually. Max possible difference is around 2.0 (1.0 to -1.0),
        // but typically smaller. Let's clamp and scale.
        if score < 0.0 {
            score = 0.0;
        } else {
            // A score of 0.3 difference is quite significant in cosine space.
            score = (score * 2.0).clamp(0.0, 1.0);
        }

        let is_pulsar = score >= 0.5;

        Ok(PulsarResult {
            is_pulsar,
            score,
            history_len: vectors.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_pulsar_no_rhythm() -> Result<()> {
        let db = AletheiaDB::new()?;
        let mut tx = db.write_transaction()?;

        // Create a node that drifts slowly
        let node_id = tx.create_node(
            "Drifter",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )?;
        tx.commit()?;

        // Drift to [0.8, 0.2]
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.8, 0.2])
                .build(),
        )?;
        tx.commit()?;

        // Drift to [0.6, 0.4]
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.6, 0.4])
                .build(),
        )?;
        tx.commit()?;

        // Drift to [0.4, 0.6]
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.4, 0.6])
                .build(),
        )?;
        tx.commit()?;

        let pulsar = Pulsar::new(&db);
        let result = pulsar.detect_rhythm(node_id, "vec", 3)?;

        assert!(!result.is_pulsar);
        assert!(result.score < 0.2);
        assert_eq!(result.history_len, 4);

        Ok(())
    }

    #[test]
    fn test_pulsar_strong_rhythm() -> Result<()> {
        let db = AletheiaDB::new()?;

        // State A: [1.0, 0.0]
        // State B: [0.0, 1.0]

        // Version 1: State A
        let mut tx = db.write_transaction()?;
        let node_id = tx.create_node(
            "Oscillator",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )?;
        tx.commit()?;

        // Version 2: State B
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0])
                .build(),
        )?;
        tx.commit()?;

        // Version 3: State A
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )?;
        tx.commit()?;

        // Version 4: State B
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0])
                .build(),
        )?;
        tx.commit()?;

        // Version 5: State A
        let mut tx = db.write_transaction()?;
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )?;
        tx.commit()?;

        let pulsar = Pulsar::new(&db);
        let result = pulsar.detect_rhythm(node_id, "vec", 3)?;

        assert!(result.is_pulsar);
        assert!(result.score > 0.8);
        assert_eq!(result.history_len, 5);

        Ok(())
    }
}
