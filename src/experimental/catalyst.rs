#![allow(clippy::collapsible_if)]

//! Catalyst: The Semantic Chain Reaction Engine 💥
//!
//! "What if a small shift in meaning triggers a global paradigm shift?"
//!
//! Catalyst detects "Tipping Points" in the knowledge graph. It simulates how
//! a localized cluster of highly synergistic concepts can overcome its boundaries
//! and cascade through the network, permanently altering the semantic landscape.
//!
//! # Concepts
//! - **Chain Reaction**: A propagation event that accelerates when it encounters
//!   structurally synergistic nodes, rather than dampening over time.
//! - **Critical Mass**: The synergy threshold required for a local cluster to
//!   "ignite" and force its emergent vector onto neighbors.
//! - **Ignition Point**: The localized node where the chain reaction starts.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::catalyst::Catalyst;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let start_node = NodeId::new(0).unwrap();
//!
//! let catalyst = Catalyst::new(&db);
//! // Ignite a reaction from `start_node` using the "embedding" property.
//! // Critical mass threshold is 0.75 synergy.
//! let impact = catalyst.ignite(start_node, "embedding", 0.75, 5)?;
//!
//! println!("Nodes permanently altered: {}", impact.altered_nodes);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use crate::experimental::synergy::Synergy;
use std::collections::{HashMap, HashSet};

/// The result of a Catalyst chain reaction.
#[derive(Debug, Clone)]
pub struct CatalystImpact {
    /// The number of nodes whose vectors were significantly altered.
    pub altered_nodes: usize,
    /// The final "emergent" vector that dominated the cascade (if critical mass was reached).
    pub dominant_vector: Option<Vec<f32>>,
    /// Did the reaction achieve critical mass?
    pub critical_mass_reached: bool,
}

/// The Catalyst Engine.
pub struct Catalyst<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Catalyst<'a> {
    /// Create a new Catalyst Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Ignite a semantic chain reaction starting from a specific node.
    ///
    /// # Arguments
    /// * `ignition_node` - The node where the reaction begins.
    /// * `property_name` - The vector property to simulate upon.
    /// * `critical_mass_threshold` - The synergy score (0.0 to 1.0) required to trigger a cascade.
    /// * `max_steps` - Maximum propagation steps to simulate.
    pub fn ignite(
        &self,
        ignition_node: NodeId,
        property_name: &str,
        critical_mass_threshold: f32,
        max_steps: usize,
    ) -> Result<CatalystImpact> {
        // 1. Initial State Load
        let mut current_state = HashMap::new();
        // Since `ReadTransaction` doesn't have a `.query()` method, we use `self.db.query()` directly outside
        let results = self.db.query().scan(None).execute(self.db)?;
        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node() {
                if let Some(prop) = node.get_property(property_name) {
                    if let Some(vec) = prop.as_vector() {
                        current_state.insert(node.id, vec.to_vec());
                    }
                }
            }
        }

        if !current_state.contains_key(&ignition_node) {
            return Err(Error::other(
                "Ignition node does not have the specified vector property.",
            ));
        }

        let synergy_engine = Synergy::new(self.db);
        let mut active_front: HashSet<NodeId> = HashSet::new();
        active_front.insert(ignition_node);

        let mut critical_mass_reached = false;
        let mut dominant_vector = None;
        let mut altered_nodes = 0;

        let mut next_state = current_state.clone();

        for _step in 0..max_steps {
            if active_front.is_empty() {
                break;
            }

            let mut next_front = HashSet::new();
            let mut cluster_to_analyze = Vec::new();

            // Gather the cluster to check for critical mass
            for &node_id in &active_front {
                cluster_to_analyze.push(node_id);
                // Also add immediate neighbors to the cluster analysis for local context
                self.db.read(|tx| {
                    let outgoing = tx.get_outgoing_edges(node_id);
                    for edge_id in outgoing {
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            cluster_to_analyze.push(edge.target);
                            next_front.insert(edge.target); // They become the next front
                        }
                    }
                    Ok::<(), Error>(())
                })?;
            }

            cluster_to_analyze.sort();
            cluster_to_analyze.dedup();

            // Retain only nodes that have state
            cluster_to_analyze.retain(|id| current_state.contains_key(id));

            if cluster_to_analyze.len() > 1 && !critical_mass_reached {
                // Check synergy of the current active cluster
                if let Ok(synergy_result) =
                    synergy_engine.analyze(&cluster_to_analyze, property_name)
                {
                    if synergy_result.synergy_score >= critical_mass_threshold {
                        critical_mass_reached = true;
                        dominant_vector = Some(synergy_result.emergent_vector.clone());
                    }
                }
            }

            // Propagation Phase
            for &node_id in &cluster_to_analyze {
                if let Some(current_vec) = current_state.get(&node_id) {
                    if critical_mass_reached {
                        // Cascade: The dominant vector overwrites local vectors rapidly
                        if let Some(ref dom_vec) = dominant_vector {
                            let mut new_vec = vec![0.0; current_vec.len()];
                            // 90% dominant, 10% original (massive overwrite)
                            for i in 0..current_vec.len() {
                                new_vec[i] = 0.1 * current_vec[i] + 0.9 * dom_vec[i];
                            }
                            ops::normalize_in_place(&mut new_vec);
                            next_state.insert(node_id, new_vec);
                            altered_nodes += 1;
                        }
                    } else {
                        // Normal slow linear propagation (diffusion)
                        // Pull from neighbors
                        let mut neighbor_vectors = Vec::new();
                        self.db.read(|tx| {
                            let incoming = tx.get_incoming_edges(node_id);
                            for edge_id in incoming {
                                if let Ok(edge) = tx.get_edge(edge_id) {
                                    if let Some(n_vec) = current_state.get(&edge.source) {
                                        neighbor_vectors.push(n_vec.clone());
                                    }
                                }
                            }
                            Ok::<(), Error>(())
                        })?;

                        if !neighbor_vectors.is_empty() {
                            let mut sum = vec![0.0; current_vec.len()];
                            for nv in &neighbor_vectors {
                                for i in 0..sum.len() {
                                    sum[i] += nv[i];
                                }
                            }
                            let count = neighbor_vectors.len() as f32;
                            let mut new_vec = vec![0.0; current_vec.len()];
                            // 80% self, 20% neighbors (slow diffusion)
                            for i in 0..current_vec.len() {
                                new_vec[i] = 0.8 * current_vec[i] + 0.2 * (sum[i] / count);
                            }
                            ops::normalize_in_place(&mut new_vec);
                            next_state.insert(node_id, new_vec);
                            // We don't count slow diffusion as an "altered node" for the impact score,
                            // only explosive cascades.
                        }
                    }
                }
            }

            current_state = next_state.clone();
            active_front = next_front;

            // Optional: Break early if all nodes are consumed by the cascade
            if critical_mass_reached && active_front.is_empty() {
                break;
            }
        }

        Ok(CatalystImpact {
            altered_nodes,
            dominant_vector,
            critical_mass_reached,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_catalyst_no_ignition() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("idea", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("idea", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(n1, n2, "RELATES_TO", Default::default())
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let catalyst = Catalyst::new(&db);
        // High threshold, should not ignite
        let impact = catalyst.ignite(n1, "idea", 0.99, 3).unwrap();

        assert!(!impact.critical_mass_reached);
        assert_eq!(impact.altered_nodes, 0);
        assert!(impact.dominant_vector.is_none());
    }

    #[test]
    fn test_catalyst_ignition_and_cascade() {
        let db = AletheiaDB::new().unwrap();

        let mut nodes = Vec::new();

        db.write(|tx| {
            // Create a cluster of diverse but connected concepts
            for i in 0..4 {
                let vec = vec![i as f32 * 0.2, 1.0 - (i as f32 * 0.2)];
                let n = tx
                    .create_node(
                        "Concept",
                        PropertyMapBuilder::new()
                            .insert_vector("idea", &vec)
                            .build(),
                    )
                    .unwrap();
                nodes.push(n);
            }

            // Interconnect them heavily to create high synergy
            tx.create_edge(nodes[0], nodes[1], "LINK", Default::default())
                .unwrap();
            tx.create_edge(nodes[1], nodes[2], "LINK", Default::default())
                .unwrap();
            tx.create_edge(nodes[2], nodes[3], "LINK", Default::default())
                .unwrap();
            tx.create_edge(nodes[3], nodes[0], "LINK", Default::default())
                .unwrap();
            tx.create_edge(nodes[0], nodes[2], "LINK", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let catalyst = Catalyst::new(&db);
        // Low threshold to force ignition.
        // Also note that synergy calculation requires more nodes to hit synergy.
        // Let's use 0.0 threshold to guarantee ignition if there's any difference.
        let impact = catalyst.ignite(nodes[0], "idea", 0.0, 5).unwrap();

        assert!(impact.critical_mass_reached);
        assert!(impact.altered_nodes > 0);
        assert!(impact.dominant_vector.is_some());
    }

    #[test]
    fn test_catalyst_missing_property() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        db.write(|tx| {
            n1 = tx.create_node("Concept", Default::default()).unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let catalyst = Catalyst::new(&db);
        let result = catalyst.ignite(n1, "missing", 0.5, 1);

        assert!(result.is_err());
    }
}
