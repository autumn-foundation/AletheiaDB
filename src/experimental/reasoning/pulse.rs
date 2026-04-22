//! Pulse: Semantic Graph Heartbeat Simulator.
//!
//! "What happens to the graph when time passes and meanings drift?"
//!
//! Pulse simulates the natural evolution of concepts over time by applying
//! drift to node vectors. It can be used to simulate a "living graph" where
//! concepts gradually change meaning (Random Walk) or converge towards a
//! central theme (Gravity Well).

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::api::transaction::WriteOps;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::property::PropertyMapBuilder;
use rand::{Rng, thread_rng};
use std::collections::HashMap;

/// Configuration for vector drift.
pub enum DriftConfig {
    /// Applies a random walk to each dimension of the vector.
    RandomWalk {
        /// The maximum magnitude of the random step.
        step_size: f32,
    },
    /// Pulls vectors towards a target point.
    GravityWell {
        /// The target vector coordinates.
        target: Vec<f32>,
        /// The fraction of the distance to move towards the target.
        pull_strength: f32,
    },
}

/// The Pulse Simulator Engine.
pub struct PulseSimulator<'a> {
    db: &'a AletheiaDB,
}

impl<'a> PulseSimulator<'a> {
    /// Creates a new PulseSimulator instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Applies a heartbeat, drifting all vectors in the specified property according to the config.
    pub fn beat(&self, property_name: &str, config: DriftConfig) -> Result<()> {
        let mut updates: HashMap<NodeId, Vec<f32>> = HashMap::new();
        let mut rng = thread_rng();

        let scan_results = self.db.execute_aql("MATCH (n) RETURN n")?;
        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                if let Some(vec) = node.get_property(property_name).and_then(|v| v.as_vector()) {
                    let mut new_vec = vec.to_vec();
                    match &config {
                        DriftConfig::RandomWalk { step_size } => {
                            for v in new_vec.iter_mut() {
                                *v += rng.gen_range(-*step_size..=*step_size);
                            }
                        }
                        DriftConfig::GravityWell {
                            target,
                            pull_strength,
                        } => {
                            if new_vec.len() == target.len() {
                                for i in 0..new_vec.len() {
                                    let diff = target[i] - new_vec[i];
                                    new_vec[i] += diff * pull_strength;
                                }
                            }
                        }
                    }
                    updates.insert(node.id, new_vec);
                }
            }
        }

        if !updates.is_empty() {
            let mut tx = self.db.write_transaction()?;
            for (node_id, new_vec) in updates {
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new()
                        .insert_vector(property_name, &new_vec)
                        .build(),
                )?;
            }
            tx.commit()?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::{ReadOps, WriteOps};
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_pulse_random_walk() {
        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let n1 = tx
            .create_node_with_valid_time("Concept", props, None)
            .unwrap();
        tx.commit().unwrap();

        let pulse = PulseSimulator::new(&db);
        let config = DriftConfig::RandomWalk { step_size: 0.1 };
        pulse.beat("vec", config).unwrap();

        let tx_read = db.read_transaction().unwrap();
        let node = tx_read.get_node(n1).unwrap();
        let vec = node.get_property("vec").unwrap().as_vector().unwrap();
        assert!(vec[0] != 0.0 || vec[1] != 0.0);
    }

    #[test]
    fn test_pulse_gravity_well() {
        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let n1 = tx
            .create_node_with_valid_time("Concept", props, None)
            .unwrap();
        tx.commit().unwrap();

        let pulse = PulseSimulator::new(&db);
        let config = DriftConfig::GravityWell {
            target: vec![1.0, 1.0],
            pull_strength: 0.5,
        };
        pulse.beat("vec", config).unwrap();

        let tx_read = db.read_transaction().unwrap();
        let node = tx_read.get_node(n1).unwrap();
        let vec = node.get_property("vec").unwrap().as_vector().unwrap();
        assert!(vec[0] > 0.0 && vec[0] <= 0.5);
        assert!(vec[1] > 0.0 && vec[1] <= 0.5);
    }
}
