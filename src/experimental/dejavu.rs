//! Dejavu: Repeating History Detection Engine 🔄
//!
//! "History doesn't repeat itself, but it often rhymes."
//!
//! The Dejavu Engine detects cyclical semantic patterns over time. It compares a node's
//! vector trajectory in one time window against its trajectory in a previous time window
//! to see if it is moving in a similar pattern.
//!
//! # Concepts
//! - **Trajectory**: The vector movement (delta) of a node within a specific time window.
//! - **Window**: A fixed duration of time.
//! - **Similarity**: Cosine similarity between two trajectory vectors.

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::cosine_similarity;

/// Represents a repeating pattern detected by the Dejavu Engine.
#[derive(Debug, Clone, PartialEq)]
pub struct DejavuResult {
    /// The similarity score between the two trajectories (0.0 to 1.0).
    pub similarity_score: f32,
    /// The trajectory vector for the older window.
    pub historical_trajectory: Vec<f32>,
    /// The trajectory vector for the newer window.
    pub recent_trajectory: Vec<f32>,
}

/// The Dejavu Engine for detecting repeating semantic patterns.
pub struct DejavuEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejavuEngine<'a> {
    /// Create a new Dejavu Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node's vector trajectory is repeating a historical pattern.
    ///
    /// Compares the trajectory (end vector - start vector) of `recent_window`
    /// with the trajectory of `historical_window` using cosine similarity.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to track.
    /// * `historical_window` - The older time window.
    /// * `recent_window` - The newer time window to compare against.
    pub fn detect_repeating_history(
        &self,
        node_id: NodeId,
        property_name: &str,
        historical_window: &TimeRange,
        recent_window: &TimeRange,
    ) -> Result<DejavuResult> {
        // We use AletheiaDB's get_node_at_time to fetch the states
        // at the start and end of both windows.

        // Transaction time is passed as identical to valid time to query exact state known then.
        let hist_start = self
            .db
            .get_node_at_time(
                node_id,
                historical_window.start(),
                historical_window.start(),
            )
            .map_err(|e| Error::other(format!("hist_start: {}", e)))?;
        let hist_end = self
            .db
            .get_node_at_time(node_id, historical_window.end(), historical_window.end())
            .map_err(|e| Error::other(format!("hist_end: {}", e)))?;

        let rec_start = self
            .db
            .get_node_at_time(node_id, recent_window.start(), recent_window.start())
            .map_err(|e| Error::other(format!("rec_start: {}", e)))?;
        let rec_end = self
            .db
            .get_node_at_time(node_id, recent_window.end(), recent_window.end())
            .map_err(|e| Error::other(format!("rec_end: {}", e)))?;

        let get_vec = |node: &crate::core::Node| -> Result<Vec<f32>> {
            node.get_property(property_name)
                .and_then(|p| p.as_vector())
                .map(|v| v.to_vec())
                .ok_or_else(|| Error::other(format!("Missing vector property '{}'", property_name)))
        };

        let hist_start_vec = get_vec(&hist_start)?;
        let hist_end_vec = get_vec(&hist_end)?;
        let rec_start_vec = get_vec(&rec_start)?;
        let rec_end_vec = get_vec(&rec_end)?;

        // Ensure dimensions match
        if hist_start_vec.len() != hist_end_vec.len()
            || rec_start_vec.len() != rec_end_vec.len()
            || hist_start_vec.len() != rec_start_vec.len()
        {
            return Err(Error::other("Dimension mismatch in vector history"));
        }

        // Calculate trajectories (delta = end - start)
        let mut hist_traj = vec![0.0; hist_start_vec.len()];
        let mut rec_traj = vec![0.0; rec_start_vec.len()];

        for i in 0..hist_start_vec.len() {
            hist_traj[i] = hist_end_vec[i] - hist_start_vec[i];
            rec_traj[i] = rec_end_vec[i] - rec_start_vec[i];
        }

        // Check for zero vectors
        let hist_mag = hist_traj.iter().map(|x| x * x).sum::<f32>().sqrt();
        let rec_mag = rec_traj.iter().map(|x| x * x).sum::<f32>().sqrt();

        let similarity = if hist_mag == 0.0 || rec_mag == 0.0 {
            // If there's no movement in one or both windows, we default to 0 similarity
            // unless both are zero (perfect stillness matches perfect stillness).
            if hist_mag == 0.0 && rec_mag == 0.0 {
                1.0
            } else {
                0.0
            }
        } else {
            cosine_similarity(&hist_traj, &rec_traj)?
        };

        Ok(DejavuResult {
            similarity_score: similarity,
            historical_trajectory: hist_traj,
            recent_trajectory: rec_traj,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_repeating_history() {
        let db = AletheiaDB::new().unwrap();

        let engine = DejavuEngine::new(&db);

        // At t0: Create node with vector [0.0, 0.0]
        let props_t0 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let mut tx0 = db.write_transaction().unwrap();
        let node_id = tx0.create_node("Node", props_t0).unwrap();
        tx0.commit().unwrap();
        let t0_time = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // At t1: Move vector to [1.0, 1.0] (Trajectory 1: +1, +1)
        let mut tx1 = db.write_transaction().unwrap();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 1.0])
            .build();
        tx1.update_node(node_id, props).unwrap();
        tx1.commit().unwrap();
        let t1_time = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // At t2: Move vector to [1.0, 0.0] (Trajectory 2: 0, -1)
        let mut tx2 = db.write_transaction().unwrap();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        tx2.update_node(node_id, props).unwrap();
        tx2.commit().unwrap();
        let t2_time = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // At t3: Move vector to [2.0, 1.0] (Trajectory 3: +1, +1) -> Repeating Trajectory 1!
        let mut tx3 = db.write_transaction().unwrap();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[2.0, 1.0])
            .build();
        tx3.update_node(node_id, props).unwrap();
        tx3.commit().unwrap();
        let t3_time = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let window_hist = TimeRange::new(t0_time, t1_time).unwrap();
        let window_recent = TimeRange::new(t2_time, t3_time).unwrap();

        let result = engine
            .detect_repeating_history(node_id, "vec", &window_hist, &window_recent)
            .unwrap();

        // Trajectories should match perfectly
        assert!((result.similarity_score - 1.0).abs() < f32::EPSILON);
        assert_eq!(result.historical_trajectory, vec![1.0, 1.0]);
        assert_eq!(result.recent_trajectory, vec![1.0, 1.0]);
    }
}
