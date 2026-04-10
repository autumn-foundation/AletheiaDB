//! Meteor: Semantic Collision Prediction Engine.
//!
//! "Brace for impact."
//!
//! The Meteor engine predicts when two concepts (nodes) will semantically collide
//! by analyzing their historical vector trajectories and extrapolating them into the future.
//!
//! # Use Cases
//! - **Trend Prediction**: Detecting when two different ideas are about to converge.
//! - **Risk Analysis**: Identifying when user behavior is drifting towards a hazardous state.
//! - **Narrative Generation**: Triggering events when character arcs intersect.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::meteor::Meteor;
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_a = db.create_node("Concept", Default::default())?;
//! # let node_b = db.create_node("Concept", Default::default())?;
//!
//! let meteor = Meteor::new(&db);
//!
//! // Predict if node_a and node_b will collide within the next 24 hours
//! // A collision is defined as their vectors being within a distance of 0.1
//! if let Some(collision_time) = meteor.predict_collision(node_a, node_b, "embedding", 0.1, Duration::from_secs(86400))? {
//!     println!("Collision imminent at: {:?}", collision_time);
//! } else {
//!     println!("Trajectories are parallel or diverging.");
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::magnitude;
use std::time::Duration;

/// Represents the velocity (rate of change) of a vector in semantic space.
#[derive(Debug, Clone)]
pub struct SemanticVelocity {
    /// The velocity vector (change per microsecond).
    pub vector: Vec<f32>,
    /// The last known position.
    pub last_position: Vec<f32>,
    /// The timestamp of the last known position.
    pub last_timestamp: Timestamp,
}

/// The Meteor engine predicts future semantic collisions.
pub struct Meteor<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Meteor<'a> {
    /// Creates a new Meteor engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Predicts if and when two nodes will collide based on their recent vector trajectories.
    ///
    /// # Arguments
    /// * `node_a` - The first node.
    /// * `node_b` - The second node.
    /// * `property` - The name of the vector property to track.
    /// * `collision_threshold` - The maximum euclidean distance to be considered a collision.
    /// * `max_horizon` - The maximum time into the future to look.
    pub fn predict_collision(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property: &str,
        collision_threshold: f32,
        max_horizon: Duration,
    ) -> Result<Option<Timestamp>> {
        // Calculate the semantic velocities for both nodes based on their history
        let vel_a_opt = self.calculate_velocity(node_a, property)?;
        let vel_b_opt = self.calculate_velocity(node_b, property)?;

        // If either node is missing history or vectors, we cannot predict
        let (vel_a, vel_b) = match (vel_a_opt, vel_b_opt) {
            (Some(a), Some(b)) => (a, b),
            _ => return Ok(None),
        };

        // Align them to the same starting time (the later of the two last timestamps)
        let now = vel_a
            .last_timestamp
            .wallclock()
            .max(vel_b.last_timestamp.wallclock());

        let pos_a = self.extrapolate_position(&vel_a, now);
        let pos_b = self.extrapolate_position(&vel_b, now);

        if pos_a.len() != pos_b.len() {
            return Ok(None); // Dimension mismatch
        }

        // Relative position: A - B
        let mut rel_pos = Vec::with_capacity(pos_a.len());
        for (a, b) in pos_a.iter().zip(pos_b.iter()) {
            rel_pos.push(a - b);
        }

        // Relative velocity: V_a - V_b
        let mut rel_vel = Vec::with_capacity(pos_a.len());
        for (va, vb) in vel_a.vector.iter().zip(vel_b.vector.iter()) {
            rel_vel.push(va - vb);
        }

        // We want to find time t > 0 such that || rel_pos + rel_vel * t || <= threshold
        // ||P + V*t||^2 = ||V||^2 * t^2 + 2(P dot V) * t + ||P||^2
        // We set this to equal threshold^2 and solve the quadratic equation for t.

        let v_sq: f32 = rel_vel.iter().map(|x| x * x).sum();

        let p_dot_v: f32 = rel_pos.iter().zip(rel_vel.iter()).map(|(p, v)| p * v).sum();

        let p_sq: f32 = rel_pos.iter().map(|x| x * x).sum();

        let r_sq = collision_threshold * collision_threshold;

        // Quadratic coefficients: a*t^2 + b*t + c = 0
        // Use f64 for precision because t can be large (microseconds) and velocities very small
        let a = v_sq as f64;
        let b = 2.0 * (p_dot_v as f64);
        let c = (p_sq as f64) - (r_sq as f64);

        // If they are currently colliding
        if c <= 0.0 {
            return Ok(Some(Timestamp::from(now)));
        }

        // If relative velocity is zero, they will never collide (parallel or stationary)
        if a == 0.0 {
            return Ok(None);
        }

        // Discriminant
        let discriminant = b * b - 4.0 * a * c;

        // If discriminant < 0, they will never collide
        if discriminant < 0.0 {
            return Ok(None);
        }

        // Find the smallest positive root
        let t1 = (-b - discriminant.sqrt()) / (2.0 * a);
        let t2 = (-b + discriminant.sqrt()) / (2.0 * a);

        let t = if t1 >= 0.0 {
            t1
        } else if t2 >= 0.0 {
            t2
        } else {
            return Ok(None); // Collision happened in the past
        };

        // Convert t (in microseconds) to Timestamp
        let t_us = t.round() as i64;

        let max_us = max_horizon.as_micros() as i64;
        if t_us > max_us {
            return Ok(None); // Collision is beyond the horizon
        }

        Ok(Some(Timestamp::from(now + t_us)))
    }

    /// Extrapolates a new position for the given time based on the semantic velocity.
    fn extrapolate_position(&self, vel: &SemanticVelocity, target_time: i64) -> Vec<f32> {
        let dt = (target_time - vel.last_timestamp.wallclock()) as f32;
        let mut new_pos = Vec::with_capacity(vel.last_position.len());
        for (p, v) in vel.last_position.iter().zip(vel.vector.iter()) {
            new_pos.push(p + v * dt);
        }
        new_pos
    }

    /// Calculates the semantic velocity by looking at the last two historical states of the node.
    fn calculate_velocity(
        &self,
        node_id: NodeId,
        property: &str,
    ) -> Result<Option<SemanticVelocity>> {
        let history = self.db.get_node_history(node_id)?;

        // Find the two most recent versions with the specified vector property
        let mut last_version = None;
        let mut prev_version = None;

        for version in history.versions.iter().rev() {
            let is_valid = version
                .properties
                .get(property)
                .and_then(|p| p.as_vector())
                .is_some();
            if is_valid {
                let vec = version
                    .properties
                    .get(property)
                    .unwrap()
                    .as_vector()
                    .unwrap();
                let ts = version.temporal.valid_time().start();
                let state = (vec.to_vec(), ts);
                if last_version.is_none() {
                    last_version = Some(state);
                } else if prev_version.is_none() {
                    prev_version = Some(state);
                    break;
                }
            }
        }

        match (last_version, prev_version) {
            (Some((last_vec, last_ts)), Some((prev_vec, prev_ts))) => {
                let dt = last_ts.wallclock() - prev_ts.wallclock();
                if dt <= 0 {
                    return Ok(None); // Cannot determine velocity
                }

                if last_vec.len() != prev_vec.len() {
                    return Ok(None); // Dimension mismatch
                }

                let dt_f32 = dt as f32;
                let mut velocity = Vec::with_capacity(last_vec.len());
                for (cur, prev) in last_vec.iter().zip(prev_vec.iter()) {
                    velocity.push((cur - prev) / dt_f32);
                }

                Ok(Some(SemanticVelocity {
                    vector: velocity,
                    last_position: last_vec,
                    last_timestamp: last_ts,
                }))
            }
            // Need at least two points to calculate velocity
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_meteor_collision_prediction() {
        let db = AletheiaDB::new().unwrap();

        // Node A starts at [0.0, 10.0] and moves to [0.0, 5.0] (moving towards origin)
        // Node B starts at [10.0, 0.0] and moves to [5.0, 0.0] (moving towards origin)

        let props_a1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 10.0])
            .build();
        let a = db.create_node("Node", props_a1).unwrap();

        let props_b1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();
        let b = db.create_node("Node", props_b1).unwrap();

        let t_0 = time::now().wallclock();
        let dt_us = 1_000_000; // 1 second step
        let t_1 = Timestamp::from(t_0 + dt_us);

        // Update positions
        let props_a2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 5.0])
            .build();
        let props_b2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[5.0, 0.0])
            .build();

        db.write(|tx| {
            tx.update_node_with_valid_time(a, props_a2, Some(t_1))?;
            tx.update_node_with_valid_time(b, props_b2, Some(t_1))
        })
        .unwrap();

        let meteor = Meteor::new(&db);

        // Predict collision with threshold 1.0 within 10 seconds
        let collision_opt = meteor
            .predict_collision(a, b, "vec", 1.0, Duration::from_secs(10))
            .unwrap();

        assert!(collision_opt.is_some(), "Should predict a collision");

        let collision_time = collision_opt.unwrap().wallclock();

        // Velocity A: [0.0, -5.0/s]
        // Velocity B: [-5.0/s, 0.0]
        // Relative Pos (A-B) at t=1s: [-5.0, 5.0]
        // Relative Vel (Va-Vb): [5.0, -5.0]
        // Distance is 0 at t=1s from t_1 (i.e., t=2s from t_0)

        let expected_time = t_1.wallclock() + dt_us;

        // The collision might happen slightly earlier due to the threshold of 1.0.
        // It should be within a small range.
        assert!(
            (collision_time - expected_time).abs() < 500_000,
            "Collision time should be around 1 second after t_1"
        );
    }

    #[test]
    fn test_meteor_no_collision_diverging() {
        let db = AletheiaDB::new().unwrap();

        // Node A starts at [0.0, 5.0] and moves to [0.0, 10.0] (moving away from origin)
        // Node B starts at [5.0, 0.0] and moves to [10.0, 0.0] (moving away from origin)

        let props_a1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 5.0])
            .build();
        let a = db.create_node("Node", props_a1).unwrap();

        let props_b1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[5.0, 0.0])
            .build();
        let b = db.create_node("Node", props_b1).unwrap();

        let t_0 = time::now().wallclock();
        let dt_us = 1_000_000;
        let t_1 = Timestamp::from(t_0 + dt_us);

        let props_a2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 10.0])
            .build();
        let props_b2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();

        db.write(|tx| {
            tx.update_node_with_valid_time(a, props_a2, Some(t_1))?;
            tx.update_node_with_valid_time(b, props_b2, Some(t_1))
        })
        .unwrap();

        let meteor = Meteor::new(&db);
        let collision_opt = meteor
            .predict_collision(a, b, "vec", 1.0, Duration::from_secs(10))
            .unwrap();

        assert!(
            collision_opt.is_none(),
            "Should not predict a collision for diverging paths"
        );
    }
}
