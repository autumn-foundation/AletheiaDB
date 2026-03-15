//! Ember: Semantic Spreading Activation Engine.
//!
//! "Where there is smoke, there is fire."
//!
//! Ember propagates a scalar "heat" (activation) value through the graph.
//! It is useful for identifying nodes that are highly relevant to a set of starting
//! active nodes, effectively simulating a spreading activation network.
//!
//! # Concepts
//! - **Heat**: A scalar value (f32) representing the activation level of a node.
//! - **Cooling Rate**: How much heat a node loses each step.
//! - **Spread Rate**: How much heat a node transfers to its neighbors.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::ember::{EmberEngine, EmberConfig};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let ember = EmberEngine::new(&db);
//!
//! let config = EmberConfig {
//!     cooling_rate: 0.1,
//!     spread_rate: 0.8,
//!     max_steps: 10,
//!     threshold: 0.01,
//! };
//!
//! // The engine propagates the "heat" property across the graph.
//! let active_nodes = ember.propagate("heat", &config)?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use std::collections::{HashMap, HashSet};

/// Configuration for the Ember engine.
#[derive(Debug, Clone)]
pub struct EmberConfig {
    /// How much heat is lost at each step (0.0 to 1.0).
    /// 0.0 means no heat is lost, 1.0 means all heat is lost instantly unless replenished.
    pub cooling_rate: f32,
    /// What fraction of a node's heat is distributed to its neighbors.
    /// 0.0 means no heat spreads.
    pub spread_rate: f32,
    /// Maximum number of propagation steps.
    pub max_steps: usize,
    /// Stop propagation if the maximum change in heat across all nodes is below this threshold.
    pub threshold: f32,
}

impl Default for EmberConfig {
    fn default() -> Self {
        Self {
            cooling_rate: 0.1,
            spread_rate: 0.8,
            max_steps: 5,
            threshold: 0.01,
        }
    }
}

/// The Ember Engine for spreading activation.
pub struct EmberEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EmberEngine<'a> {
    /// Create a new EmberEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Propagates heat through the graph based on the `property_name`.
    ///
    /// This runs a simulation in memory and returns the final heat distribution.
    /// It does not automatically write the new heat values back to the database.
    ///
    /// # Arguments
    /// * `property_name` - The property storing the initial heat (must be a float or int).
    /// * `config` - Simulation parameters.
    pub fn propagate(
        &self,
        property_name: &str,
        config: &EmberConfig,
    ) -> Result<HashMap<NodeId, f32>> {
        let mut current_heat = self.load_initial_heat(property_name)?;

        if current_heat.is_empty() {
            return Ok(HashMap::new());
        }

        for _step in 0..config.max_steps {
            let mut next_heat = HashMap::new();
            let mut max_change = 0.0_f32;

            // Determine which nodes to process. We need to process nodes that currently have heat,
            // AND their neighbors.
            let mut active_nodes: HashSet<NodeId> = current_heat.keys().cloned().collect();
            let mut neighbor_expansion = Vec::new();

            // We use a read closure to avoid borrowing issues
            self.db.read(|tx| {
                for &node_id in &active_nodes {
                    let edges = tx.get_outgoing_edges(node_id);
                    for edge_id in edges {
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            neighbor_expansion.push(edge.target);
                        }
                    }
                }
                Ok::<(), Error>(())
            })?;

            for target in neighbor_expansion {
                active_nodes.insert(target);
            }

            self.db.read(|tx| {
                for &node_id in &active_nodes {
                    let old_heat = *current_heat.get(&node_id).unwrap_or(&0.0);

                    // 1. Calculate received heat from neighbors
                    let mut received_heat = 0.0;

                    let incoming = tx.get_incoming_edges(node_id);
                    for edge_id in incoming {
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            let source = edge.source;
                            if let Some(&source_heat) = current_heat.get(&source) {
                                // How much heat does the source give?
                                // Let's say it distributes config.spread_rate of its heat equally among its outgoing edges.
                                // For simplicity, we need to know the source's out-degree.
                                let out_degree = tx.get_outgoing_edges(source).len().max(1) as f32;
                                let heat_transfer = (source_heat * config.spread_rate) / out_degree;
                                received_heat += heat_transfer;
                            }
                        }
                    }

                    // 2. Retained heat (after spreading and cooling)
                    let retained_heat =
                        old_heat * (1.0 - config.spread_rate) * (1.0 - config.cooling_rate);

                    // 3. New heat
                    let new_heat = retained_heat + received_heat;

                    if new_heat > 0.0 {
                        next_heat.insert(node_id, new_heat);
                    }

                    let diff = (new_heat - old_heat).abs();
                    if diff > max_change {
                        max_change = diff;
                    }
                }
                Ok::<(), Error>(())
            })?;

            current_heat = next_heat;

            if max_change < config.threshold {
                break;
            }
        }

        Ok(current_heat)
    }

    fn load_initial_heat(&self, property_name: &str) -> Result<HashMap<NodeId, f32>> {
        let mut heat_map = HashMap::new();
        // A full scan is needed if we don't have an index on the scalar property.
        let results = self.db.query().scan(None).execute(self.db)?;

        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node()
                && let Some(prop) = node.get_property(property_name)
                && let Some(val) = prop.as_float().or_else(|| prop.as_int().map(|i| i as f64))
                && val > 0.0
            {
                heat_map.insert(node.id, val as f32);
            }
        }
        Ok(heat_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_ember_spreading_activation() {
        let db = AletheiaDB::new().unwrap();

        // Node A (Heat: 100) -> Node B (Heat: 0) -> Node C (Heat: 0)
        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("heat", 100.0).build(),
            )
            .unwrap();

        let b = db.create_node("Node", Default::default()).unwrap();
        let c = db.create_node("Node", Default::default()).unwrap();

        db.create_edge(a, b, "LINKS", Default::default()).unwrap();
        db.create_edge(b, c, "LINKS", Default::default()).unwrap();

        let engine = EmberEngine::new(&db);
        let config = EmberConfig {
            cooling_rate: 0.0, // No heat lost to the environment
            spread_rate: 0.5,  // 50% retained, 50% spread
            max_steps: 1,
            threshold: 0.0,
        };

        // Step 1:
        // A starts with 100.
        // A retains 50. Spreads 50 to B.
        // B receives 50.
        // C receives 0 (B had 0 initially).
        let state1 = engine.propagate("heat", &config).unwrap();
        assert_eq!(*state1.get(&a).unwrap_or(&0.0), 50.0);
        assert_eq!(*state1.get(&b).unwrap_or(&0.0), 50.0);
        assert_eq!(*state1.get(&c).unwrap_or(&0.0), 0.0);

        let config2 = EmberConfig {
            max_steps: 2,
            ..config
        };

        // Step 2:
        // A retains 50% of 50 = 25. Spreads 25 to B.
        // B retains 50% of 50 = 25. Receives 25 from A. Spreads 25 to C.
        // B total = 25 + 25 = 50.
        // C retains 0. Receives 25 from B.
        let state2 = engine.propagate("heat", &config2).unwrap();

        assert_eq!(*state2.get(&a).unwrap_or(&0.0), 25.0);
        assert_eq!(*state2.get(&b).unwrap_or(&0.0), 50.0);
        assert_eq!(*state2.get(&c).unwrap_or(&0.0), 25.0);
    }

    #[test]
    fn test_ember_cooling() {
        let db = AletheiaDB::new().unwrap();

        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("heat", 100.0).build(),
            )
            .unwrap();

        let engine = EmberEngine::new(&db);
        let config = EmberConfig {
            cooling_rate: 0.1, // 10% lost
            spread_rate: 0.0,  // No spread
            max_steps: 1,
            threshold: 0.0,
        };

        let state = engine.propagate("heat", &config).unwrap();
        assert_eq!(*state.get(&a).unwrap_or(&0.0), 90.0);
    }
}
