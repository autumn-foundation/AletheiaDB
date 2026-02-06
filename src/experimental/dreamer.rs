//! Dreamer: Semantic Trajectory Extrapolation.
//!
//! "Where is this concept going?"
//!
//! This module analyzes the historical evolution of a node's vector embedding
//! to predict its future position in semantic space. It can answer questions like:
//! - "If 'Apple' moved from 'Fruit' to 'Tech', where will it be in 5 years?"
//! - "Based on user behavior trends, what will they be interested in next?"
//!
//! # How it works
//! 1. **Fetch History**: Retrieves historical versions of a node's vector property.
//! 2. **Calculate Velocity**: Computes the average "semantic velocity" (change per second).
//! 3. **Extrapolate**: Projects the vector forward in time.
//! 4. **Search**: Finds the nearest neighbors to the predicted future vector.

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::utils::{Error, Result, VectorError};
use std::time::Duration;

/// The Dreamer engine for predictive semantic analysis.
pub struct Dreamer<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Dreamer<'a> {
    /// Create a new Dreamer instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Predict the future semantic neighbors of a node.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property` - The vector property name (must be indexed).
    /// * `history_window` - The time range to analyze for trajectory calculation.
    /// * `future_horizon` - How far into the future to project from the last known point.
    /// * `k` - Number of neighbors to find.
    ///
    /// # Returns
    /// A list of `(NodeId, score)` pairs representing the nodes closest to the predicted future state.
    pub fn predict_future(
        &self,
        node_id: NodeId,
        property: &str,
        history_window: TimeRange,
        future_horizon: Duration,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // 1. Fetch History
        let history = self.db.get_node_history(node_id)?;

        // Extract vector snapshots: (timestamp_micros, vector)
        let mut snapshots: Vec<(i64, Vec<f32>)> = Vec::new();

        for version in &history.versions {
            let valid_time = version.temporal.valid_time();

            // Check if version is relevant to the window
            // We include versions that start within the window or overlap it significantly.
            // Simplest is to check if valid_start is inside the window.
            if valid_time.start() < history_window.end()
                && valid_time.end() > history_window.start()
            {
                // Get the property value from this version
                if let Some(prop_val) = version.properties.get(property)
                    && let Some(vec) = prop_val.as_vector()
                {
                    // Use the max of (valid_start, window_start) as the effective timestamp
                    // ensuring we don't look back before the window started.
                    let effective_time = valid_time
                        .start()
                        .wallclock()
                        .max(history_window.start().wallclock());

                    // Avoid duplicates if multiple versions map to same effective time?
                    // Just take them all, sort later.
                    snapshots.push((effective_time, vec.to_vec()));
                }
            }
        }

        // Sort by time to ensure chronological order
        snapshots.sort_by_key(|(t, _)| *t);

        // Deduplicate timestamps (keep latest per timestamp if any)
        snapshots.dedup_by_key(|(t, _)| *t);

        if snapshots.is_empty() {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "No vector history found for node {} in property '{}' within the specified window",
                node_id, property
            ))));
        }

        // 2. Calculate Trajectory (Velocity)
        let (last_time, last_vec) = snapshots.last().unwrap();

        let projected_vec = if snapshots.len() < 2 {
            // Not enough history to determine trajectory. Assume static.
            last_vec.clone()
        } else {
            let (first_time, first_vec) = snapshots.first().unwrap();

            let duration_micros = last_time - first_time;

            if duration_micros <= 0 {
                last_vec.clone()
            } else {
                let duration_secs = duration_micros as f32 / 1_000_000.0;
                let horizon_secs = future_horizon.as_secs_f32();

                // Linear projection: Future = Last + (Velocity * Horizon)
                // Velocity = (Last - First) / Duration
                let mut proj = Vec::with_capacity(last_vec.len());

                for (start, end) in first_vec.iter().zip(last_vec.iter()) {
                    let velocity = (end - start) / duration_secs;
                    let future_val = end + (velocity * horizon_secs);
                    proj.push(future_val);
                }
                proj
            }
        };

        // 3. Search
        self.db.search_vectors_in(property, &projected_vec, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_dreamer_trajectory_extrapolation() {
        let db = AletheiaDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Euclidean); // 2D for simplicity
        db.enable_vector_index("embedding", config).unwrap();

        let t_start = time::now();

        // Target Nodes:
        // 1. Novice [0, 0]
        // 2. Intermediate [5, 5]
        // 3. Expert [10, 10] (This is where we predict it goes)

        let _novice = db
            .create_node(
                "Level",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let _inter = db
            .create_node(
                "Level",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[5.0, 5.0])
                    .build(),
            )
            .unwrap();
        let expert = db
            .create_node(
                "Level",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[10.0, 10.0])
                    .build(),
            )
            .unwrap();

        // The Learner Node
        // Starts at [0, 0]
        let learner_props = PropertyMapBuilder::new()
            .insert("name", "Learner")
            .insert_vector("embedding", &[0.0, 0.0])
            .build();
        let learner = db.create_node("Student", learner_props).unwrap();

        // Wait to establish time gap
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _t_mid = time::now();

        // Update Learner to [5, 5]
        // This simulates moving towards "Expert"
        let update_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[5.0, 5.0])
            .build();
        db.write(|tx| tx.update_node(learner, update_props))
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let t_end = time::now();

        // Verify history exists
        let history = db.get_node_history(learner).unwrap();
        assert!(history.version_count() >= 2);

        // Run Dreamer
        let dreamer = Dreamer::new(&db);

        // Window covers start to end
        let window = TimeRange::new(t_start, t_end).unwrap();

        // Calculate duration between versions roughly
        let duration_micros = t_end.wallclock() - t_start.wallclock();
        let _duration = Duration::from_micros(duration_micros as u64);

        // Predict forward by same duration.
        // Logic: 0 -> 5 in `duration`.
        // In another `duration`, it should be at 10.
        // But t_mid is when update happened.
        // The first version is at T_creation (near t_start).
        // The second version is at T_update (near t_mid).
        // Time diff is approx 50ms.
        // We project 50ms into future from T_update.

        // Let's use a generic future horizon
        let horizon = Duration::from_millis(50);

        let predictions = dreamer
            .predict_future(learner, "embedding", window, horizon, 1)
            .unwrap();

        assert!(!predictions.is_empty());

        // Should find "Expert" node [10, 10]
        assert_eq!(
            predictions[0].0, expert,
            "Dreamer should predict the learner becomes an expert"
        );
    }

    #[test]
    fn test_dreamer_static_trajectory() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        let t0 = time::now();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 1.0])
            .build();
        let node = db.create_node("Node", props).unwrap();
        let t1 = time::now();

        // No updates.
        let dreamer = Dreamer::new(&db);
        let window = TimeRange::new(t0, t1).unwrap();

        // Should return same position
        let res = dreamer
            .predict_future(node, "vec", window, Duration::from_secs(10), 1)
            .unwrap();

        // Should match itself (as it's the closest to [1.0, 1.0])
        assert_eq!(res[0].0, node);
    }
}
