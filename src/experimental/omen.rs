//! Omen: A predictive model for node property transitions.
//!
//! Omen analyzes the historical transitions of node properties to build a
//! Markov Chain-based predictive model. It can answer questions like:
//! "If a node has `status: 'Pending'`, what is the most likely next status?"
//!
//! # Features
//! - **Transition Probability**: Calculates the likelihood of transitioning from state A to state B.
//! - **Duration Estimation**: Estimates the average time spent in state A before transitioning.
//! - **Incremental Training**: Can be trained on subsets of nodes incrementally.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::core::property::PropertyValue;
//! use aletheiadb::experimental::omen::Omen;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... Assume nodes with history exist ...
//! let nodes = vec![/* ... */];
//!
//! let mut omen = Omen::new();
//! omen.train(&db, &nodes, "status")?;
//!
//! let current_status = PropertyValue::from("Pending");
//! let predictions = omen.predict(&current_status)?;
//!
//! for prediction in predictions {
//!     println!(
//!         "Next: {:?} (Probability: {:.2}%, Avg Duration: {:?})",
//!         prediction.next_value,
//!         prediction.probability * 100.0,
//!         prediction.avg_duration
//!     );
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;
use std::collections::HashMap;
use std::time::Duration;

/// A prediction for the next state of a property.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    /// The predicted next value.
    pub next_value: PropertyValue,
    /// The probability of this transition (0.0 - 1.0).
    pub probability: f32,
    /// The average duration spent in the current state before this transition.
    pub avg_duration: Duration,
}

/// Internal statistics for a specific state.
#[derive(Debug, Clone, Default)]
struct StateStats {
    /// Total number of transitions observed from this state.
    total_transitions: usize,
    /// Map of next state (serialized) -> TransitionStats.
    transitions: HashMap<Vec<u8>, TransitionStats>,
}

/// Statistics for a specific transition (A -> B).
#[derive(Debug, Clone, Default)]
struct TransitionStats {
    /// Number of times this transition occurred.
    count: usize,
    /// Total duration spent in state A before transitioning to B.
    total_duration_micros: u128,
}

/// The Omen predictive model.
#[derive(Debug, Default)]
pub struct Omen {
    /// Map from current state (serialized) -> StateStats.
    model: HashMap<Vec<u8>, StateStats>,
}

impl Omen {
    /// Create a new, empty Omen model.
    pub fn new() -> Self {
        Self::default()
    }

    /// Train the model on the history of the specified nodes for a specific property key.
    ///
    /// This method iterates through the history of each node, identifies value transitions
    /// for the given property key, and updates the transition statistics.
    ///
    /// # Arguments
    /// * `db` - The database instance.
    /// * `nodes` - A slice of NodeIds to train on.
    /// * `key` - The property key to analyze (e.g., "status").
    pub fn train(&mut self, db: &AletheiaDB, nodes: &[NodeId], key: &str) -> Result<()> {
        for &node_id in nodes {
            let history = db.get_node_history(node_id)?;

            // We need at least 2 versions to see a transition
            if history.version_count() < 2 {
                continue;
            }

            // Sort versions by valid time start to reconstruct the timeline
            let mut versions = history.versions.clone();
            versions.sort_by(|a, b| {
                a.temporal
                    .valid_time()
                    .start()
                    .cmp(&b.temporal.valid_time().start())
            });

            // Iterate through consecutive versions
            for i in 0..versions.len() - 1 {
                let current_version = &versions[i];
                let next_version = &versions[i + 1];

                // Get values for the property key
                let current_val = current_version.properties.get(key);
                let next_val = next_version.properties.get(key);

                // We only care if the property exists in both versions and has changed
                if let (Some(curr), Some(next)) = (current_val, next_val) {
                    // Skip if values are semantically equal (no transition)
                    if curr.semantically_equal(next) {
                        continue;
                    }

                    // Calculate duration of the current state
                    // Duration is the difference between valid_start of next version and valid_start of current version
                    // This assumes the transition happened exactly when the next version became valid.
                    let start_micros = current_version.temporal.valid_time().start().wallclock();
                    let end_micros = next_version.temporal.valid_time().start().wallclock();

                    let duration_micros = if end_micros > start_micros {
                        (end_micros - start_micros) as u128
                    } else {
                        0 // Should not happen if sorted, but safety first
                    };

                    self.record_transition(curr, next, duration_micros)?;
                }
            }
        }
        Ok(())
    }

    /// Predict the likely next states given a current value.
    ///
    /// Returns a list of predictions sorted by probability (descending).
    ///
    /// # Arguments
    /// * `current_value` - The current value of the property.
    pub fn predict(&self, current_value: &PropertyValue) -> Result<Vec<Prediction>> {
        let serialized_curr = current_value.serialize()?;

        let Some(stats) = self.model.get(&serialized_curr) else {
            return Ok(Vec::new());
        };

        if stats.total_transitions == 0 {
            return Ok(Vec::new());
        }

        let mut predictions = Vec::new();

        for (serialized_next, trans_stats) in &stats.transitions {
            let (next_value, _) = PropertyValue::deserialize(serialized_next)?;

            let probability = trans_stats.count as f32 / stats.total_transitions as f32;

            let avg_micros = if trans_stats.count > 0 {
                (trans_stats.total_duration_micros / trans_stats.count as u128) as u64
            } else {
                0
            };

            predictions.push(Prediction {
                next_value,
                probability,
                avg_duration: Duration::from_micros(avg_micros),
            });
        }

        // Sort by probability descending
        predictions.sort_by(|a, b| {
            b.probability
                .partial_cmp(&a.probability)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(predictions)
    }

    /// Record a single transition in the model.
    fn record_transition(
        &mut self,
        from: &PropertyValue,
        to: &PropertyValue,
        duration_micros: u128,
    ) -> Result<()> {
        let serialized_from = from.serialize()?;
        let serialized_to = to.serialize()?;

        let state_stats = self.model.entry(serialized_from).or_default();
        state_stats.total_transitions += 1;

        let trans_stats = state_stats.transitions.entry(serialized_to).or_default();
        trans_stats.count += 1;
        trans_stats.total_duration_micros += duration_micros;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_omen_linear_transition() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().insert("status", "A").build();
        let node = db.create_node("Task", props).unwrap();

        // A -> B
        // Wait a bit to simulate duration
        std::thread::sleep(Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new().insert("status", "B").build(),
            )
        })
        .unwrap();

        let mut omen = Omen::new();
        omen.train(&db, &[node], "status").unwrap();

        let predictions = omen.predict(&PropertyValue::from("A")).unwrap();

        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].next_value.as_str(), Some("B"));
        assert!((predictions[0].probability - 1.0).abs() < f32::EPSILON);
        assert!(predictions[0].avg_duration.as_millis() >= 9); // Allow some jitter
    }

    #[test]
    fn test_omen_branching_transition() {
        let db = AletheiaDB::new().unwrap();

        // Node 1: A -> B
        let node1 = db
            .create_node(
                "Task",
                PropertyMapBuilder::new().insert("status", "A").build(),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        db.write(|tx| {
            tx.update_node(
                node1,
                PropertyMapBuilder::new().insert("status", "B").build(),
            )
        })
        .unwrap();

        // Node 2: A -> C
        let node2 = db
            .create_node(
                "Task",
                PropertyMapBuilder::new().insert("status", "A").build(),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        db.write(|tx| {
            tx.update_node(
                node2,
                PropertyMapBuilder::new().insert("status", "C").build(),
            )
        })
        .unwrap();

        let mut omen = Omen::new();
        omen.train(&db, &[node1, node2], "status").unwrap();

        let predictions = omen.predict(&PropertyValue::from("A")).unwrap();

        // Should have B and C, each with 0.5 probability
        assert_eq!(predictions.len(), 2);

        let prob_b = predictions
            .iter()
            .find(|p| p.next_value.as_str() == Some("B"))
            .unwrap()
            .probability;
        let prob_c = predictions
            .iter()
            .find(|p| p.next_value.as_str() == Some("C"))
            .unwrap()
            .probability;

        assert!((prob_b - 0.5).abs() < f32::EPSILON);
        assert!((prob_c - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_omen_no_transitions() {
        let db = AletheiaDB::new().unwrap();
        let node = db
            .create_node(
                "Task",
                PropertyMapBuilder::new().insert("status", "A").build(),
            )
            .unwrap();

        let mut omen = Omen::new();
        omen.train(&db, &[node], "status").unwrap();

        let predictions = omen.predict(&PropertyValue::from("A")).unwrap();
        assert!(predictions.is_empty());
    }

    #[test]
    fn test_omen_unknown_state() {
        let omen = Omen::new();
        let predictions = omen.predict(&PropertyValue::from("Unknown")).unwrap();
        assert!(predictions.is_empty());
    }

    #[test]
    fn test_omen_ignore_same_state_updates() {
        let db = AletheiaDB::new().unwrap();
        let node = db
            .create_node(
                "Task",
                PropertyMapBuilder::new().insert("status", "A").build(),
            )
            .unwrap();

        // Update A -> A (should be ignored)
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new().insert("status", "A").build(),
            )
        })
        .unwrap();

        // Update A -> B
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new().insert("status", "B").build(),
            )
        })
        .unwrap();

        let mut omen = Omen::new();
        omen.train(&db, &[node], "status").unwrap();

        let predictions = omen.predict(&PropertyValue::from("A")).unwrap();

        // Should only see A -> B transition
        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].next_value.as_str(), Some("B"));
        assert_eq!(predictions[0].probability, 1.0);
    }
}
