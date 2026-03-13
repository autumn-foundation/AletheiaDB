#![allow(clippy::collapsible_if)]
//! Momentum: Semantic Velocity & Acceleration Engine 🚀
//!
//! "Is this concept evolving steadily towards a new meaning, or just fluctuating randomly?"
//!
//! The Momentum Engine analyzes the sequence of historical vectors for a node to
//! calculate its semantic velocity and acceleration.
//!
//! # Concepts
//! - **Semantic Velocity**: The average rate of change (distance) of a node's vector over time steps.
//! - **Semantic Acceleration**: The change in velocity between consecutive time steps.
//! - **Directional Momentum**: High velocity + low acceleration (steady movement) + high alignment.
//! - **Semantic Oscillation**: High acceleration (constantly changing speed/direction).
//!
//! # Example
//!
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::momentum::MomentumEngine;
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let node_id = db.create_node("Test", Default::default())?;
//!
//! let engine = MomentumEngine::new(&db);
//!
//! let start = time::now() - 3600 * 1_000_000 * 24 * 7;
//! let end = time::now();
//!
//! let result = engine.analyze_momentum(node_id, "embedding", start, end)?;
//!
//! if result.is_directional {
//!     println!("Node has strong directional momentum!");
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::euclidean_distance;

/// Result of a momentum analysis.
#[derive(Debug, Clone)]
pub struct MomentumResult {
    /// The average distance moved per time step.
    pub velocity: f32,
    /// The average change in velocity per time step.
    pub acceleration: f32,
    /// The total distance moved from start to end.
    pub total_displacement: f32,
    /// The sum of distances moved in each step.
    pub path_length: f32,
    /// True if the node is moving steadily in one direction (path length is close to displacement).
    pub is_directional: bool,
    /// True if the node is changing rapidly but not moving far (high path length, low displacement).
    pub is_oscillating: bool,
}

/// The Momentum Engine.
pub struct MomentumEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> MomentumEngine<'a> {
    /// Create a new Momentum Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze the semantic momentum of a node over a time window.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use.
    /// * `start` - The start of the time window.
    /// * `end` - The end of the time window.
    /// * `steps` - The number of time slices to take within the window (must be >= 2).
    pub fn analyze_momentum(
        &self,
        node_id: NodeId,
        property_name: &str,
        start: Timestamp,
        end: Timestamp,
    ) -> Result<MomentumResult> {
        let history = self.db.get_node_history(node_id)?;
        let mut historical_vectors = Vec::new();

        for version in history.versions {
            if version.temporal.valid_time().start() >= start
                && version.temporal.valid_time().start() <= end
            {
                if let Some(prop) = version
                    .properties
                    .get(property_name)
                    .and_then(|p| p.as_vector())
                {
                    historical_vectors.push(prop.to_vec());
                }
            }
        }

        if historical_vectors.len() < 2 {
            return Err(Error::other("Not enough historical data points found"));
        }

        let mut step_distances = Vec::new();
        for i in 0..historical_vectors.len() - 1 {
            let dist = euclidean_distance(&historical_vectors[i], &historical_vectors[i + 1])?;
            step_distances.push(dist);
        }

        let mut path_length = 0.0;
        for &dist in &step_distances {
            path_length += dist;
        }

        let velocity = path_length / step_distances.len() as f32;

        let mut accel_sum = 0.0;
        if step_distances.len() > 1 {
            for i in 0..step_distances.len() - 1 {
                accel_sum += (step_distances[i + 1] - step_distances[i]).abs();
            }
        }
        let acceleration = if step_distances.len() > 1 {
            accel_sum / (step_distances.len() - 1) as f32
        } else {
            0.0
        };

        let total_displacement = euclidean_distance(
            &historical_vectors[0],
            &historical_vectors[historical_vectors.len() - 1],
        )?;

        // Directionality: path_length should be close to total_displacement if moving straight
        // Oscillation: path_length is much larger than total_displacement
        let epsilon = 1e-5;
        let directionality_ratio = total_displacement / (path_length + epsilon);

        let is_directional = directionality_ratio > 0.8 && total_displacement > 0.1;
        let is_oscillating = directionality_ratio < 0.2 && path_length > 0.1;

        Ok(MomentumResult {
            velocity,
            acceleration,
            total_displacement,
            path_length,
            is_directional,
            is_oscillating,
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
    fn test_directional_momentum() {
        let db = AletheiaDB::new().unwrap();

        let t1 = time::now();
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

        std::thread::sleep(std::time::Duration::from_millis(50));
        let _t2 = time::now();

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

        std::thread::sleep(std::time::Duration::from_millis(50));
        let _t3 = time::now();

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

        let engine = MomentumEngine::new(&db);
        let end = time::now();
        let result = engine
            .analyze_momentum(node_id, "embedding", t1, end)
            .unwrap();

        assert!(result.is_directional);
        assert!(!result.is_oscillating);
        assert!((result.total_displacement - 2.0).abs() < 1e-4);
        assert!((result.path_length - 2.0).abs() < 1e-4);
        assert!((result.velocity - 1.0).abs() < 1e-4);
        assert!(result.acceleration < 1e-4);
    }

    #[test]
    fn test_oscillation() {
        let db = AletheiaDB::new().unwrap();

        let t1 = time::now();
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

        std::thread::sleep(std::time::Duration::from_millis(50));
        let _t2 = time::now();

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

        std::thread::sleep(std::time::Duration::from_millis(50));
        let _t3 = time::now();

        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 0.0]) // Moves back
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let engine = MomentumEngine::new(&db);
        let end = time::now();
        let result = engine
            .analyze_momentum(node_id, "embedding", t1, end)
            .unwrap();

        assert!(!result.is_directional);
        assert!(result.is_oscillating);
        assert!(result.total_displacement < 1e-4); // Started at 0, ended at 0
        assert!((result.path_length - 2.0).abs() < 1e-4); // Moved 1 out, 1 back
    }
}
