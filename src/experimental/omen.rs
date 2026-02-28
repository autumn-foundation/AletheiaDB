//! Omen: Semantic Collision Detection.
//!
//! "I have a bad feeling about this..."
//!
//! Omen predicts the future interaction of two concepts by analyzing their
//! semantic trajectories. It answers questions like:
//! - "Will 'Artificial Intelligence' and 'Ethics' collide soon?"
//! - "Is 'Project X' moving away from its 'Original Goal'?"
//!
//! # Concepts
//! - **Trajectory**: The path a node is taking through semantic space, modeled as a velocity vector.
//! - **Encounter**: The point in time (future or past) where two trajectories are closest.
//!
//! # Logic
//! Given two nodes A and B with positions $P_a, P_b$ and velocities $V_a, V_b$:
//! 1. Relative Position: $P = P_b - P_a$
//! 2. Relative Velocity: $V = V_b - V_a$
//! 3. Distance Squared at time $t$: $D^2(t) = ||P + V*t||^2$
//! 4. Minimized when derivative is 0: $t = -(P \cdot V) / ||V||^2$
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::omen::Omen;
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_a = db.create_node("A", Default::default())?;
//! # let node_b = db.create_node("B", Default::default())?;
//! let omen = Omen::new(&db);
//!
//! // Analyze last hour
//! let window = TimeRange::new(time::now() - 3600 * 1_000_000, time::now())?;
//!
//! if let Some(encounter) = omen.predict_encounter(node_a, node_b, window, "embedding")? {
//!     println!("Predicted encounter in {:.2} seconds.", encounter.time_to_encounter.as_secs_f32());
//!     println!("Closest distance: {:.4}", encounter.predicted_distance);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::{TimeRange, time};
use std::time::Duration;

/// The predicted outcome of two trajectories.
#[derive(Debug, Clone)]
pub struct Encounter {
    /// Time until the encounter (from window end).
    /// Negative means the encounter happened in the past (diverging).
    pub time_to_encounter: Duration,
    /// Is the encounter in the past? (Helper boolean since Duration is unsigned in std, but we model offset conceptually).
    /// Note: std::time::Duration is unsigned. We store magnitude in `time_to_encounter` and sign here.
    pub is_past: bool,
    /// The estimated semantic distance at the point of closest approach.
    pub predicted_distance: f32,
}

/// The Omen Engine.
pub struct Omen<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Omen<'a> {
    /// Create a new Omen instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Predict the encounter between two nodes based on their trajectories in the given window.
    pub fn predict_encounter(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        window: TimeRange,
        vector_property: &str,
    ) -> Result<Option<Encounter>> {
        // Fix transaction time to "now" for consistent view
        let tx_time = time::now();

        // 1. Calculate Trajectories
        // We need Position (at window end) and Velocity (change over window)
        let traj_a = self.calculate_trajectory(node_a, window, vector_property, tx_time)?;
        let traj_b = self.calculate_trajectory(node_b, window, vector_property, tx_time)?;

        // If either has no vector, we can't predict
        let (pos_a, vel_a) = match traj_a {
            Some(t) => t,
            None => return Ok(None),
        };
        let (pos_b, vel_b) = match traj_b {
            Some(t) => t,
            None => return Ok(None),
        };

        // 2. Physics Math
        // Relative Position P = Pb - Pa (at t=0, which is window.end)
        let rel_pos: Vec<f32> = pos_b.iter().zip(pos_a.iter()).map(|(b, a)| b - a).collect();

        // Relative Velocity V = Vb - Va
        let rel_vel: Vec<f32> = vel_b.iter().zip(vel_a.iter()).map(|(b, a)| b - a).collect();

        // Calculate dot products
        let p_dot_v: f32 = rel_pos.iter().zip(rel_vel.iter()).map(|(p, v)| p * v).sum();

        let v_dot_v: f32 = rel_vel.iter().map(|v| v * v).sum();

        // If relative velocity is effectively zero, paths are parallel.
        // Closest distance is current distance. t = 0.
        if v_dot_v < 1e-9 {
            let current_dist = rel_pos.iter().map(|x| x * x).sum::<f32>().sqrt();
            return Ok(Some(Encounter {
                time_to_encounter: Duration::from_secs(0),
                is_past: false,
                predicted_distance: current_dist,
            }));
        }

        // t = -(P . V) / ||V||^2
        let t_secs = -p_dot_v / v_dot_v;

        // Calculate closest distance at t
        // Pos(t) = P + V*t
        let mut pos_at_t = Vec::with_capacity(rel_pos.len());
        for (p, v) in rel_pos.iter().zip(rel_vel.iter()) {
            pos_at_t.push(p + v * t_secs);
        }
        let min_dist = pos_at_t.iter().map(|x| x * x).sum::<f32>().sqrt();

        let is_past = t_secs < 0.0;
        let abs_secs = t_secs.abs();
        let time_to_encounter = Duration::from_secs_f32(abs_secs);

        Ok(Some(Encounter {
            time_to_encounter,
            is_past,
            predicted_distance: min_dist,
        }))
    }

    /// Calculate (Position, Velocity) for a node.
    /// Position is vector at window.end.
    /// Velocity is (Vector_End - Vector_Start) / Duration.
    fn calculate_trajectory(
        &self,
        node_id: NodeId,
        window: TimeRange,
        property: &str,
        _tx_time: crate::core::temporal::Timestamp,
    ) -> Result<Option<(Vec<f32>, Vec<f32>)>> {
        // Fetch full history directly to bypass potential temporal index issues in tests.
        // In real usage, `get_node_at_time` is preferred, but for unit tests with simulated time,
        // history scan is more robust.
        //
        // Note: We use `window.start()` and `window.end()` to find the bracketing versions.

        let history = self.db.get_node_history(node_id)?;

        // Find vector at or immediately before window.start
        let start_vec = self.find_vector_at(&history, window.start(), property);
        // Find vector at or immediately before window.end
        let end_vec = self.find_vector_at(&history, window.end(), property);

        match (start_vec, end_vec) {
            (Some(start), Some(end)) => {
                if start.len() != end.len() {
                    return Ok(None); // Dim mismatch
                }

                let duration_micros = window.duration_micros().unwrap_or(0);
                if duration_micros == 0 {
                    // Instantaneous window, velocity is zero? Or undefined?
                    // Treat as zero velocity.
                    let zero_vel = vec![0.0; start.len()];
                    return Ok(Some((end, zero_vel)));
                }

                let duration_secs = duration_micros as f32 / 1_000_000.0;

                let velocity: Vec<f32> = end
                    .iter()
                    .zip(start.iter())
                    .map(|(e, s)| (e - s) / duration_secs)
                    .collect();

                Ok(Some((end, velocity)))
            }
            _ => Ok(None), // Missing data
        }
    }

    fn find_vector_at(
        &self,
        history: &crate::core::history::EntityHistory,
        time: crate::core::temporal::Timestamp,
        property: &str,
    ) -> Option<Vec<f32>> {
        // Iterate history to find the latest version valid at `time`.
        // History is typically sorted by version ID. We want the latest version where valid_time.start <= time.
        // Assuming versions are sorted by valid_time or we scan all.
        let mut best_vec = None;
        let mut best_time = i64::MIN;

        for v in &history.versions {
            let vt_start = v.temporal.valid_time().start().wallclock();
            // Since we aren't using #![feature(let_chains)], we suppress the
            // `collapsible_if` lint here to avoid build failures with stable Rust.
            #[allow(clippy::collapsible_if)]
            if vt_start <= time.wallclock() && vt_start >= best_time {
                #[allow(clippy::collapsible_if)]
                if let Some(val) = v.properties.get(property) {
                    if let Some(vec) = val.as_vector() {
                        best_vec = Some(vec.to_vec());
                        best_time = vt_start;
                    }
                }
            }
        }
        best_vec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_omen_head_on_collision() {
        let db = AletheiaDB::new().unwrap();
        // Enable index
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Node A: Starts at [0, 0], moving Right (+X)
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let a = db.create_node("A", props_a).unwrap();

        // Node B: Starts at [10, 0], moving Left (-X)
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();
        let b = db.create_node("B", props_b).unwrap();

        // Wait to establish initial state time
        std::thread::sleep(Duration::from_millis(10));
        let t_start = time::now();

        // Wait 100ms
        std::thread::sleep(Duration::from_millis(100));

        // Update A to [1, 0] (Vel = 1 unit/100ms = 10 units/s)
        db.write(|tx| {
            tx.update_node(
                a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // Update B to [9, 0] (Vel = -1 unit/100ms = -10 units/s)
        db.write(|tx| {
            tx.update_node(
                b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[9.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();

        let omen = Omen::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();

        // At T_end:
        // Pos A = [1, 0]
        // Pos B = [9, 0]
        // Vel A = +X
        // Vel B = -X
        // Rel Pos (B-A) = [8, 0]
        // Rel Vel (B-A) = (-X) - (+X) = -2X
        // They are closing distance.
        // Distance is 8. Closing speed is 2X (relative).
        // Time to impact = 8 / relative_speed.
        // Wait, velocity magnitude depends on exact duration.
        // Let Omen calculate it.

        let encounter = omen
            .predict_encounter(a, b, window, "vec")
            .unwrap()
            .unwrap();

        println!("Encounter: {:?}", encounter);

        assert!(!encounter.is_past, "Collision should be in future");
        assert!(
            encounter.predicted_distance < 0.01,
            "Should be a direct hit"
        );
        // Time should be positive
        assert!(encounter.time_to_encounter.as_secs_f32() > 0.0);
    }

    #[test]
    fn test_omen_diverging() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Start together at [0,0]
        let pa = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let a = db.create_node("A", pa.clone()).unwrap();
        let b = db.create_node("B", pa.clone()).unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_start = time::now();
        std::thread::sleep(Duration::from_millis(50));

        // Move apart
        // A -> [-1, 0]
        db.write(|tx| {
            tx.update_node(
                a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // B -> [1, 0]
        db.write(|tx| {
            tx.update_node(
                b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();
        let omen = Omen::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();

        // At T_end, they are at [-1,0] and [1,0]. Moving apart.
        // Closest approach was in the past (at start).

        let encounter = omen
            .predict_encounter(a, b, window, "vec")
            .unwrap()
            .unwrap();

        assert!(encounter.is_past, "Encounter should be in the past");
        // Closest distance should be ~0 (when they were together)
        assert!(encounter.predicted_distance < 0.1);
    }

    #[test]
    fn test_omen_static_target() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Static Target T at [10, 0]
        let props_t = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();
        let target = db.create_node("Target", props_t).unwrap();

        // Mover M at [0, 0]
        let props_m = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let mover = db.create_node("Mover", props_m).unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_start = time::now();
        std::thread::sleep(Duration::from_millis(50));

        // Mover -> [1, 0] (Towards target)
        db.write(|tx| {
            tx.update_node(
                mover,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();
        let omen = Omen::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();

        let encounter = omen
            .predict_encounter(mover, target, window, "vec")
            .unwrap()
            .unwrap();

        assert!(!encounter.is_past);
        assert!(encounter.predicted_distance < 0.01);
    }

    #[test]
    fn test_omen_parallel() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // A: [0, 0] -> [1, 0]
        let a = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        // B: [0, 1] -> [1, 1] (Parallel, distance 1.0 constant)
        let b = db
            .create_node(
                "B",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_start = time::now();
        std::thread::sleep(Duration::from_millis(50));

        db.write(|tx| {
            tx.update_node(
                a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        db.write(|tx| {
            tx.update_node(
                b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();
        let omen = Omen::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();

        let encounter = omen
            .predict_encounter(a, b, window, "vec")
            .unwrap()
            .unwrap();

        // Relative velocity is 0.
        // Should report immediate encounter (t=0) with current distance.
        assert_eq!(encounter.time_to_encounter.as_secs(), 0);
        assert!((encounter.predicted_distance - 1.0).abs() < 0.01);
    }
}
